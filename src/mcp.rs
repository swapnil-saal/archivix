//! Model Context Protocol server (JSON-RPC 2.0 over stdio).
//!
//! Lets AI agents inspect archives without extracting them: list members,
//! read a member (text or hex preview), grep inside an archive, get stats,
//! and extract or create archives when they need files on disk.

use crate::extract::{ExtractOptions, Matcher};
use crate::{CreateOptions, Entry};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

const PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "archive_list",
            "description": "List the members of an archive (zip, tar, tar.gz/zst/xz/bz2/lz4/br, 7z, rar, gz/zst/xz single files) WITHOUT extracting. Returns path, kind, size, mtime, compressed size and method per member. Supports glob filtering and pagination.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string", "description": "Archive file path"},
                "pattern": {"type": "string", "description": "Optional glob or exact member name, e.g. '*.py' or 'src/**' or 'docs'"},
                "offset": {"type": "integer", "default": 0},
                "limit": {"type": "integer", "default": 500},
                "password": {"type": "string"}
            }, "required": ["path"]}
        },
        {
            "name": "archive_info",
            "description": "Summary of an archive: format, member counts, total uncompressed size, archive size, ratio, and capabilities (instant listing, random access, parallel decode).",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "password": {"type": "string"}}, "required": ["path"]}
        },
        {
            "name": "archive_read",
            "description": "Read one member's content directly from the archive without extracting to disk. Text is returned as-is (UTF-8); binary content is returned as a hex dump preview. Use offset/max_bytes to page through large members.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "member": {"type": "string", "description": "Exact member path as shown by archive_list"},
                "offset": {"type": "integer", "default": 0},
                "max_bytes": {"type": "integer", "default": 65536},
                "password": {"type": "string"}
            }, "required": ["path", "member"]}
        },
        {
            "name": "archive_grep",
            "description": "Search text members of an archive for a regular expression without extracting. Returns matching lines with member path and line number.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "regex": {"type": "string"},
                "members": {"type": "string", "description": "Optional glob limiting which members are searched, e.g. '*.rs'"},
                "ignore_case": {"type": "boolean", "default": false},
                "max_matches": {"type": "integer", "default": 200},
                "password": {"type": "string"}
            }, "required": ["path", "regex"]}
        },
        {
            "name": "archive_extract",
            "description": "Extract an archive (or selected members) to a directory, in parallel and with path-traversal protection.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "dest": {"type": "string"},
                "members": {"type": "array", "items": {"type": "string"}, "description": "Optional member names, directories or globs"},
                "strip_components": {"type": "integer", "default": 0},
                "password": {"type": "string"}
            }, "required": ["path", "dest"]}
        },
        {
            "name": "archive_create",
            "description": "Create an archive from files/directories using all CPU cores. Format follows the output extension: .zip, .tar, .tar.gz/.tgz, .tar.zst, .tar.xz, .tar.bz2, .tar.lz4, .tar.br, .7z. Prefer .tar.zst for speed with instant listing.",
            "inputSchema": {"type": "object", "properties": {
                "output": {"type": "string"},
                "inputs": {"type": "array", "items": {"type": "string"}},
                "level": {"type": "integer"},
                "exclude": {"type": "array", "items": {"type": "string"}},
                "base_dir": {"type": "string", "description": "Resolve inputs relative to this directory; member names stay relative (like tar -C)"}
            }, "required": ["output", "inputs"]}
        }
    ])
}

fn s<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a.get(k).and_then(|v| v.as_str()).with_context(|| format!("missing string argument '{k}'"))
}
fn opt_s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str())
}
fn int(a: &Value, k: &str, d: u64) -> u64 {
    a.get(k).and_then(|v| v.as_u64()).unwrap_or(d)
}

fn hexdump(data: &[u8], base: u64) -> String {
    let mut out = String::new();
    for (i, row) in data.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", base + (i * 16) as u64));
        for b in row {
            out.push_str(&format!("{b:02x} "));
        }
        for _ in row.len()..16 {
            out.push_str("   ");
        }
        out.push(' ');
        out.extend(row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
        out.push('\n');
    }
    out
}

fn call(name: &str, a: &Value) -> Result<Value> {
    let pw = opt_s(a, "password");
    match name {
        "archive_list" => {
            let path = PathBuf::from(s(a, "path")?);
            let (format, entries) = crate::list(&path, pw)?;
            let m = Matcher::new(&opt_s(a, "pattern").map(|p| vec![p.to_string()]).unwrap_or_default())?;
            let matched: Vec<&Entry> = entries.iter().filter(|e| m.matches(&e.path)).collect();
            let off = int(a, "offset", 0) as usize;
            let lim = int(a, "limit", 500) as usize;
            let page: Vec<&Entry> = matched.iter().skip(off).take(lim).copied().collect();
            Ok(
                json!({"format": format.name(), "total": matched.len(), "offset": off, "returned": page.len(), "truncated": off + page.len() < matched.len(), "entries": page}),
            )
        }
        "archive_info" => Ok(serde_json::to_value(crate::info(&PathBuf::from(s(a, "path")?), pw)?)?),
        "archive_read" => {
            let path = PathBuf::from(s(a, "path")?);
            let member = s(a, "member")?.to_string();
            let off = int(a, "offset", 0);
            let max = int(a, "max_bytes", 65536).min(4 << 20);
            let mut result = None;
            let want = member.trim_start_matches("./").to_string();
            let is_raw = matches!(crate::format::detect(&path)?, crate::Format::Raw(_));
            let filter = |e: &Entry| is_raw || e.path == want;
            let mut each = |e: &Entry, r: &mut dyn Read| -> Result<bool> {
                std::io::copy(&mut r.take(off), &mut std::io::sink())?;
                let mut buf = Vec::with_capacity(max as usize);
                r.take(max).read_to_end(&mut buf)?;
                let binary = memchr::memchr(0, &buf[..buf.len().min(8192)]).is_some()
                    || std::str::from_utf8(&buf).is_err()
                        && String::from_utf8_lossy(&buf).matches('\u{FFFD}').count() > buf.len() / 100 + 1;
                let content = if binary { hexdump(&buf[..buf.len().min(4096)], off) } else { String::from_utf8_lossy(&buf).into_owned() };
                result = Some(
                    json!({"member": e.path, "size": e.size, "offset": off, "returned_bytes": buf.len(), "truncated": off + (buf.len() as u64) < e.size, "binary": binary, "content": content}),
                );
                Ok(false)
            };
            crate::visit(&path, pw, &mut crate::Visitor { filter: &filter, each: &mut each })?;
            result.ok_or_else(|| anyhow!("member '{member}' not found"))
        }
        "archive_grep" => {
            let path = PathBuf::from(s(a, "path")?);
            let re = regex::bytes::RegexBuilder::new(s(a, "regex")?)
                .case_insensitive(a.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false))
                .build()?;
            let m = Matcher::new(&opt_s(a, "members").map(|p| vec![p.to_string()]).unwrap_or_default())?;
            let hits = crate::grep(&path, &re, &m, int(a, "max_matches", 200) as usize, pw)?;
            Ok(json!({"matches": hits.len(), "results": hits}))
        }
        "archive_extract" => {
            let members: Vec<String> = a
                .get("members")
                .and_then(|v| v.as_array())
                .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let opts = ExtractOptions {
                dest: PathBuf::from(s(a, "dest")?),
                patterns: members,
                strip_components: int(a, "strip_components", 0) as usize,
                password: pw.map(String::from),
                ..Default::default()
            };
            Ok(serde_json::to_value(crate::extract(&PathBuf::from(s(a, "path")?), &opts)?)?)
        }
        "archive_create" => {
            let inputs: Vec<PathBuf> = a
                .get("inputs")
                .and_then(|v| v.as_array())
                .context("inputs must be an array")?
                .iter()
                .filter_map(|x| x.as_str().map(PathBuf::from))
                .collect();
            let opts = CreateOptions {
                level: a.get("level").and_then(|v| v.as_i64()).map(|v| v as i32),
                base_dir: opt_s(a, "base_dir").map(PathBuf::from),
                excludes: a
                    .get("exclude")
                    .and_then(|v| v.as_array())
                    .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                ..Default::default()
            };
            Ok(serde_json::to_value(crate::create(&PathBuf::from(s(a, "output")?), &inputs, &opts)?)?)
        }
        _ => Err(anyhow!("unknown tool '{name}'")),
    }
}

fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let v = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL);
            Ok(json!({
                "protocolVersion": v,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "archivix", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Use archive_list / archive_read / archive_grep to inspect archives without extracting them. Paths are local filesystem paths."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args) {
                Ok(v) => Ok(
                    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "structuredContent": v, "isError": false}),
                ),
                Err(e) => Ok(json!({"content": [{"type": "text", "text": format!("error: {e:#}")}], "isError": true})),
            }
        }
        m if m.starts_with("notifications/") => return None,
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    let id = id?; // notifications get no response
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    })
}

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let r: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!r.is_empty()).then_some(Value::Array(r))
            }
            Ok(msg) => handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{}", serde_json::to_string(&r)?)?;
            out.flush()?;
        }
    }
    Ok(())
}
