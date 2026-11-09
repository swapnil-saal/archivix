#!/usr/bin/env python3
"""Build docs/archivix-guide.pdf, the installation and user guide.

    python3 -m venv .venv && .venv/bin/pip install reportlab
    .venv/bin/python docs/build-guide.py

Inputs: docs/icon-256.png and docs/screenshot-browse.png. Keep the command
reference in sync with `archivix <command> --help` when the CLI changes.
"""

from pathlib import Path

from reportlab.lib import colors
from reportlab.lib.enums import TA_CENTER
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import mm
from reportlab.platypus import (
    CondPageBreak,
    Image,
    KeepTogether,
    PageBreak,
    Paragraph,
    Preformatted,
    SimpleDocTemplate,
    Spacer,
    Table,
    TableStyle,
)

VERSION = "0.1.0"
REPO = "https://github.com/swapnil-saal/archivix"
DOCS = Path(__file__).resolve().parent
OUT = DOCS / "archivix-guide.pdf"

ACCENT = colors.HexColor("#4F5BE8")
TEAL = colors.HexColor("#14B8A6")
INK = colors.HexColor("#1E2230")
MUTED = colors.HexColor("#5B6175")
CODE_BG = colors.HexColor("#F3F4F8")
RULE = colors.HexColor("#DADDEA")
NOTE_BG = colors.HexColor("#EEF0FD")

PAGE_W, PAGE_H = A4
MARGIN = 20 * mm
# Usable width: the page body minus the frame's default 6 pt padding on each side.
# Boxes wider than this do not "fit", which makes KeepTogether break pages early.
BODY_W = PAGE_W - 2 * MARGIN - 12

# ----------------------------------------------------------------- styles

base = dict(fontName="Helvetica", fontSize=10, leading=14.5, textColor=INK)
S = {
    "body": ParagraphStyle("body", **base, spaceAfter=6),
    "muted": ParagraphStyle("muted", **{**base, "textColor": MUTED, "fontSize": 9, "leading": 13}),
    "h1": ParagraphStyle("h1", **{**base, "fontName": "Helvetica-Bold", "fontSize": 20, "leading": 25, "textColor": INK}, spaceBefore=4, spaceAfter=10),
    "h2": ParagraphStyle("h2", **{**base, "fontName": "Helvetica-Bold", "fontSize": 13.5, "leading": 18, "textColor": ACCENT}, spaceBefore=12, spaceAfter=5),
    "h3": ParagraphStyle("h3", **{**base, "fontName": "Helvetica-Bold", "fontSize": 11, "leading": 15}, spaceBefore=8, spaceAfter=3),
    "cell": ParagraphStyle("cell", **{**base, "fontSize": 9, "leading": 12.5}),
    "cellb": ParagraphStyle("cellb", **{**base, "fontName": "Helvetica-Bold", "fontSize": 9, "leading": 12.5}),
    "code": ParagraphStyle("code", fontName="Courier", fontSize=8.6, leading=11.8, textColor=INK),
    "bullet": ParagraphStyle("bullet", **base, leftIndent=12, bulletIndent=2, spaceAfter=2),
    "toc": ParagraphStyle("toc", **{**base, "fontSize": 11, "leading": 19}),
}


def esc(t: str) -> str:
    return t.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def md(t: str) -> str:
    """Tiny inline markup: `code` and **bold** (text is escaped first)."""
    # The PDF base fonts (WinAnsi) have no arrows; fail loudly instead of printing blanks.
    if "\u2192" in t:
        raise SystemExit(f"use 'then' or '>' instead of an arrow: {t}")
    out, code, bold = [], False, False
    t = esc(t)
    i = 0
    while i < len(t):
        if t[i] == "`":
            out.append('<font name="Courier" color="#3B44B8">' if not code else "</font>")
            code = not code
            i += 1
        elif t.startswith("**", i) and not code:
            out.append("<b>" if not bold else "</b>")
            bold = not bold
            i += 2
        else:
            out.append(t[i])
            i += 1
    return "".join(out)


def P(t, style="body"):
    return Paragraph(md(t), S[style])


def H1(t):
    return [CondPageBreak(60 * mm), Paragraph(esc(t), S["h1"])]


def H2(t):
    return [CondPageBreak(40 * mm), Paragraph(esc(t), S["h2"])]


def H3(t):
    return Paragraph(esc(t), S["h3"])


def bullets(items):
    return [Paragraph(md(i), S["bullet"], bulletText="•") for i in items]


MAX_CODE_COLS = 88  # Courier 8.6 pt fits ~89 columns in the code box


def code(text: str):
    text = text.strip("\n")
    for line in text.splitlines():
        if len(line) > MAX_CODE_COLS:
            raise SystemExit(f"code line too long ({len(line)} > {MAX_CODE_COLS}): {line}")
    pre = Preformatted(text, S["code"])
    t = Table([[pre]], colWidths=[BODY_W])
    t.setStyle(
        TableStyle(
            [
                ("BACKGROUND", (0, 0), (-1, -1), CODE_BG),
                ("BOX", (0, 0), (-1, -1), 0.6, RULE),
                ("LEFTPADDING", (0, 0), (-1, -1), 9),
                ("RIGHTPADDING", (0, 0), (-1, -1), 9),
                ("TOPPADDING", (0, 0), (-1, -1), 7),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 7),
            ]
        )
    )
    # A one-cell table never splits, so no KeepTogether is needed (nesting
    # KeepTogether inside another one makes ReportLab break pages early).
    t.spaceAfter = 7
    return t


def note(text: str, title="Note"):
    t = Table([[Paragraph(f"<b>{esc(title)}.</b> " + md(text), S["cell"])]], colWidths=[BODY_W])
    t.setStyle(
        TableStyle(
            [
                ("BACKGROUND", (0, 0), (-1, -1), NOTE_BG),
                ("LINEBEFORE", (0, 0), (0, -1), 3, ACCENT),
                ("LEFTPADDING", (0, 0), (-1, -1), 10),
                ("RIGHTPADDING", (0, 0), (-1, -1), 10),
                ("TOPPADDING", (0, 0), (-1, -1), 7),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 7),
            ]
        )
    )
    return KeepTogether([t, Spacer(1, 8)])


def table(rows, widths, header=True):
    data = []
    for r, row in enumerate(rows):
        data.append([Paragraph(md(c), S["cellb" if header and r == 0 else "cell"]) for c in row])
    t = Table(data, colWidths=[w * BODY_W for w in widths], repeatRows=1 if header else 0)
    style = [
        ("VALIGN", (0, 0), (-1, -1), "TOP"),
        ("LINEBELOW", (0, 0), (-1, -1), 0.4, RULE),
        ("TOPPADDING", (0, 0), (-1, -1), 4),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 4),
        ("LEFTPADDING", (0, 0), (-1, -1), 5),
    ]
    if header:
        style += [("BACKGROUND", (0, 0), (-1, 0), CODE_BG), ("LINEBELOW", (0, 0), (-1, 0), 0.8, ACCENT)]
    t.setStyle(TableStyle(style))
    return [t, Spacer(1, 8)]


def command(name, summary, usage, options, examples):
    head = KeepTogether([Paragraph(f'<font name="Courier-Bold" color="#4F5BE8">{esc(name)}</font>', S["h3"]), P(summary), code(usage)])
    flow = [CondPageBreak(55 * mm), head]
    if options:
        flow += table([["Option", "Meaning"]] + options, [0.34, 0.66])
    flow.append(code(examples))
    return flow


# ----------------------------------------------------------------- page decoration


def cover_page(c, doc):
    c.saveState()
    c.setFillColor(ACCENT)
    c.rect(0, PAGE_H - 9 * mm, PAGE_W, 9 * mm, stroke=0, fill=1)
    c.setFillColor(TEAL)
    c.rect(0, PAGE_H - 9 * mm, PAGE_W * 0.35, 9 * mm, stroke=0, fill=1)
    c.setFont("Helvetica", 8.5)
    c.setFillColor(MUTED)
    c.drawCentredString(PAGE_W / 2, 14 * mm, f"Licensed MIT OR Apache-2.0  ·  {REPO}")
    c.restoreState()


def later_pages(c, doc):
    c.saveState()
    c.setStrokeColor(RULE)
    c.setLineWidth(0.5)
    c.line(MARGIN, PAGE_H - 13 * mm, PAGE_W - MARGIN, PAGE_H - 13 * mm)
    c.setFont("Helvetica", 8)
    c.setFillColor(MUTED)
    c.drawString(MARGIN, PAGE_H - 11 * mm, f"archivix {VERSION}  —  Installation & User Guide")
    c.drawRightString(PAGE_W - MARGIN, 11 * mm, str(doc.page))
    c.restoreState()


# ----------------------------------------------------------------- content


def build():
    story = []

    # Cover
    story += [Spacer(1, 48 * mm)]
    icon = DOCS / "icon-256.png"
    if icon.exists():
        img = Image(str(icon), width=34 * mm, height=34 * mm)
        img.hAlign = "CENTER"
        story += [img, Spacer(1, 10 * mm)]
    story += [
        Paragraph("archivix", ParagraphStyle("t", fontName="Helvetica-Bold", fontSize=40, leading=46, alignment=TA_CENTER, textColor=INK)),
        Spacer(1, 4 * mm),
        Paragraph("Installation &amp; User Guide", ParagraphStyle("st", fontName="Helvetica", fontSize=17, leading=22, alignment=TA_CENTER, textColor=ACCENT)),
        Spacer(1, 10 * mm),
        Paragraph(
            "A parallel archive engine for zip, tar.gz, tar.zst, tar.xz, tar.bz2, tar.lz4, 7z and rar. "
            "Create, extract, list, read and search archives using every CPU core, from the command line, "
            "a desktop app, or an AI agent.",
            ParagraphStyle("d", **{**base, "fontSize": 11, "leading": 16, "alignment": TA_CENTER, "textColor": MUTED}),
        ),
        Spacer(1, 14 * mm),
        Paragraph(f"Version {VERSION}", ParagraphStyle("v", **{**base, "alignment": TA_CENTER, "textColor": MUTED})),
        PageBreak(),
    ]

    # Contents
    story += [Paragraph("Contents", S["h1"])]
    toc = [
        "1.  What archivix is",
        "2.  Installation: macOS, Windows, Linux, from source",
        "3.  Quick start",
        "4.  Command reference",
        "5.  Patterns, levels and choosing a format",
        "6.  The desktop app",
        "7.  Using archivix from scripts and AI agents",
        "8.  Troubleshooting",
    ]
    story += [Paragraph(esc(t), S["toc"]) for t in toc]
    story += [PageBreak()]

    # 1. What it is
    story += H1("1. What archivix is")
    story += [
        P(
            "archivix is one tool for every common archive format. It is built for speed: compression and "
            "extraction run on all CPU cores, and archives it creates in `.tar.gz`, `.tar.zst` and `.tar.lz4` "
            "carry a small hidden index that makes listing and reading single files instant. Those archives "
            "stay fully compatible with the standard tools (`tar`, `gzip`, `zstd`, `lz4`)."
        ),
        P("It comes in three forms that share the same engine:"),
    ]
    story += bullets(
        [
            "**archivix**: the command-line tool (this guide's main subject).",
            "**archivix-gui**: a small desktop app for macOS, Windows and Linux.",
            "**archivix mcp**: a Model Context Protocol server so AI agents can look inside archives.",
        ]
    )
    story += [Spacer(1, 4), H3("Supported formats")]
    story += table(
        [
            ["Format", "Create", "Extract", "Listing"],
            ["zip (also jar, apk, docx, whl, …)", "yes (parallel)", "yes, incl. encrypted", "instant"],
            ["tar", "yes", "yes", "instant"],
            ["tar.zst / tar.gz / tar.lz4", "yes (parallel)", "yes", "instant for archivix-made files"],
            ["tar.xz / tar.bz2", "yes (multithreaded)", "yes", "reads the stream"],
            ["tar.br / tar.lzma", "yes", "yes", "reads the stream"],
            ["7z", "yes (multithreaded LZMA2)", "yes, incl. encrypted", "instant"],
            ["rar (v1.5–v5)", "no (proprietary format)", "yes", "instant"],
            ["single files: .gz .zst .xz .bz2 .lz4 .br", "yes", "yes", "—"],
        ],
        [0.36, 0.22, 0.2, 0.22],
    )
    story += [P("Formats are recognised by their content, not their file extension, so mislabelled files still open.")]

    # 2. Installation
    story += H1("2. Installation")
    story += [
        P(f"Prebuilt downloads are on the releases page: `{REPO}/releases`. Each download has a matching `.sha256` checksum file."),
    ]
    story += table(
        [
            ["You have", "Download"],
            ["Mac (Apple Silicon or Intel), desktop app", f"`archivix-{VERSION}-macos-universal.dmg`"],
            ["Mac, command line, Apple Silicon", f"`archivix-v{VERSION}-aarch64-apple-darwin.tar.gz`"],
            ["Mac, command line, Intel", f"`archivix-v{VERSION}-x86_64-apple-darwin.tar.gz`"],
            ["Windows 10/11 (64-bit)", f"`archivix-v{VERSION}-x86_64-pc-windows-msvc.zip`"],
            ["Linux x86_64", f"`archivix-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz`"],
            ["Linux ARM64 (e.g. Raspberry Pi 4/5, Graviton)", f"`archivix-v{VERSION}-aarch64-unknown-linux-gnu.tar.gz`"],
        ],
        [0.45, 0.55],
    )
    story += [P("The Windows and Linux downloads contain both the command-line tool (`archivix`) and the desktop app (`archivix-gui`).")]

    story += H2("macOS")
    story += [H3("Desktop app"), P("Open the `.dmg` and drag **archivix** onto the **Applications** folder.")]
    story += [
        note(
            "The app is signed but not notarized by Apple, so the first launch of a downloaded copy is blocked. "
            "Right-click the app in Applications and choose **Open**, then **Open** again. Or run once in Terminal: "
            "`xattr -dr com.apple.quarantine /Applications/archivix.app`",
            "First launch",
        )
    ]
    story += [H3("Command-line tool"), P("Download the `.tar.gz` for your Mac, then in Terminal:")]
    story += [
        code(
            f"""
cd ~/Downloads
tar -xzf archivix-v{VERSION}-aarch64-apple-darwin.tar.gz
cd archivix-v{VERSION}-aarch64-apple-darwin
xattr -d com.apple.quarantine archivix      # allow the downloaded binary to run
sudo install -m 755 archivix /usr/local/bin/
archivix --version
"""
        )
    ]

    story += H2("Windows")
    story += bullets(
        [
            "Right-click the `.zip` and choose **Extract All**.",
            "Double-click `archivix-gui.exe` to start the desktop app.",
            "If SmartScreen says *Windows protected your PC*, click **More info**, then **Run anyway** (the app is not code-signed).",
        ]
    )
    story += [P("To use the command line from any terminal, copy `archivix.exe` to a folder on your PATH. In PowerShell, inside the extracted folder:")]
    story += [
        code(
            r"""
$dir = "$env:USERPROFILE\bin"
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item archivix.exe $dir
$user = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$user;$dir", "User")
# open a new terminal, then:
archivix --version
"""
        )
    ]

    story += H2("Linux")
    story += [P("Install for your user (no root needed), including a menu entry and icon for the desktop app:")]
    story += [
        code(
            f"""
tar -xzf archivix-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz
cd archivix-v{VERSION}-x86_64-unknown-linux-gnu
install -D -m 755 -t ~/.local/bin archivix archivix-gui
install -D -m 644 archivix.desktop ~/.local/share/applications/archivix.desktop
install -D -m 644 archivix.png ~/.local/share/icons/hicolor/256x256/apps/archivix.png
archivix --version        # if not found, add ~/.local/bin to your PATH
"""
        )
    ]
    story += [P("The binaries need only the standard C/C++ runtime. The desktop app also needs a graphical session (X11 or Wayland).", "muted")]

    story += H2("From source (any platform)")
    story += [P("Requires Rust 1.93 or newer (`https://rustup.rs`) and a C/C++ compiler (Xcode Command Line Tools, Visual Studio Build Tools, or gcc/clang).")]
    story += [
        code(
            f"""
# command-line tool
cargo install --git {REPO}
# desktop app
cargo install --git {REPO} archivix-gui

# or from a clone (builds target/release/archivix and archivix-gui):
git clone {REPO} && cd archivix
cargo build --release --workspace
gui/macos/bundle.sh       # macOS only: build archivix.app and a .dmg
"""
        )
    ]
    story += [
        note(
            "RAR extraction compiles RARLAB's freeware UnRAR code, which is not under an open-source license. "
            "To build without it, add `--no-default-features`. Everything else keeps working.",
            "RAR support",
        )
    ]
    story += H2("Verify a download (optional)")
    story += [
        code(
            f"""
# macOS / Linux: prints "OK" when the file is intact
shasum -a 256 -c archivix-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256

# Windows (PowerShell): compare the hash with the .sha256 file
Get-FileHash archivix-v{VERSION}-x86_64-pc-windows-msvc.zip -Algorithm SHA256
"""
        )
    ]

    # 3. Quick start
    story += H1("3. Quick start")
    story += [P("Five commands cover most use. The format of a new archive follows the name you give it.")]
    story += [
        code(
            """
archivix c project.tar.zst src/ docs/    # create (fast, instant listing)
archivix ls project.tar.zst              # list what is inside
archivix cat project.tar.zst src/main.rs # print one file without extracting
archivix x project.tar.zst -C out/       # extract into out/
archivix t project.tar.zst               # verify integrity
"""
        )
    ]
    story += [
        P(
            "In a terminal, long operations show a live progress line with percentage, speed, file count and time remaining. "
            "Each command ends with a one-line summary, for example:"
        ),
        code("project.tar.zst: 12125 entries, 585.1 MiB -> 238.7 MiB (40.8%) in 1.36s (430.2 MiB/s)"),
        P("Every command has a short alias: `c` create, `x` extract, `ls` list, `t` test, `z` compress, `d` decompress."),
    ]

    # 4. Reference
    story += H1("4. Command reference")
    story += [P("Run `archivix <command> --help` for the same information in your terminal. These options work with every command:")]
    story += table(
        [
            ["Global option", "Meaning"],
            ["`-j N`, `--threads N`", "Use N worker threads (default: all CPU cores)."],
            ["`-q`, `--quiet`", "Hide the summary line."],
            ["`-h`, `--help` / `-V`, `--version`", "Show help / version."],
        ],
        [0.34, 0.66],
    )

    story += command(
        "create  (alias: c)",
        "Create an archive from files and folders. The format is chosen from the output name: `.zip`, `.tar`, `.tar.zst`, `.tar.gz`/`.tgz`, `.tar.xz`, `.tar.bz2`, `.tar.lz4`, `.tar.br`, `.7z`.",
        "archivix create <OUTPUT> <INPUTS>... [options]",
        [
            ["`-l`, `--level N`", "Compression level, in the codec's own scale (see section 5)."],
            ["`-x`, `--exclude GLOB`", "Skip matching files or folders; repeatable."],
            ["`-C`, `--cd DIR`", "Read inputs relative to DIR, like `tar -C`."],
            ["`-F`, `--format FMT`", "Force a format instead of using the extension."],
            ["`-m`, `--method M`", "zip only: `deflate` (default), `zstd`, `store`, `bzip2`, `xz`."],
            ["`-L`, `--follow`", "Follow symbolic links instead of storing them."],
            ["`--chunk SIZE`", "Parallel chunk size, e.g. `4M` (advanced)."],
            ["`--no-index`", "Do not add the archivix index to gz/zst/lz4 archives."],
            ["`--no-smart-store`", "zip: compress everything, even photos/videos that will not shrink."],
        ],
        """
archivix c backup.zip ~/Documents
archivix c site.tgz -C /var/www html        # stores html/... (not /var/www/...)
archivix c src.tar.zst . -x node_modules -x .git -x '*.o'
archivix c small.tar.xz data/ -l 9
archivix c photos.7z Pictures/
""",
    )
    story += command(
        "extract  (alias: x)",
        "Extract everything, or only the given names, folders or glob patterns. Paths that would escape the destination folder are refused.",
        "archivix extract <ARCHIVE> [MEMBERS]... [options]",
        [
            ["`-C`, `--dest DIR`", "Destination folder (default: current folder)."],
            ["`--strip-components N`", "Drop the first N folders from each path."],
            ["`-k`, `--keep`", "Do not overwrite existing files."],
            ["`-p`, `--password PW`", "Password for encrypted zip or 7z."],
            ["`--no-perms`", "Do not restore file permissions."],
            ["`--json`", "Print a JSON summary."],
        ],
        """
archivix x project.zip -C out/
archivix x project.tar.zst 'src/**/*.rs'             # only Rust files under src/
archivix x project.tar.gz docs                       # a whole folder
archivix x release.tgz --strip-components 1
archivix x secret.7z -p 'my password'
""",
    )
    story += command(
        "list  (aliases: ls, l)",
        "List the contents without extracting. Optionally filter by names or patterns.",
        "archivix list <ARCHIVE> [MEMBERS]... [-l] [--json] [-p PW]",
        [["`-l`, `--long`", "Show permissions, size, packed size and date."], ["`--json`", "Machine-readable output."]],
        """
archivix ls project.tar.zst
archivix ls -l project.zip '*.md'
archivix ls --json project.7z > contents.json
""",
    )
    story += command(
        "cat",
        "Write the contents of one or more files to standard output, without extracting the archive. For archivix-made tar.gz/tar.zst only the needed part is decompressed.",
        "archivix cat <ARCHIVE> <MEMBERS>... [-p PW]",
        None,
        """
archivix cat project.tgz README.md
archivix cat logs.zip app/today.log | grep ERROR
""",
    )
    story += command(
        "grep",
        "Search text files inside an archive for a regular expression. Binary files are skipped. Exits with status 1 when nothing matches.",
        "archivix grep <PATTERN> <ARCHIVE> [MEMBERS]... [options]",
        [
            ["`-i`, `--ignore-case`", "Case-insensitive match."],
            ["`-m`, `--max N`", "Stop after N matches (default 1000)."],
            ["`--json`", "Machine-readable results."],
        ],
        """
archivix grep 'TODO|FIXME' project.tar.zst '*.rs'
archivix grep -i 'password' configs.zip
""",
    )
    story += command(
        "info",
        "Show the format, number of files and folders, sizes, compression ratio, and whether instant listing, random access and parallel decoding are available.",
        "archivix info <ARCHIVE> [--json] [-p PW]",
        None,
        "archivix info project.tar.gz",
    )
    story += command(
        "test  (alias: t)",
        "Decode every file and verify its checksum (CRC-32 or the stream checksum). Exits with status 2 if problems are found.",
        "archivix test <ARCHIVE> [--json] [-p PW]",
        None,
        "archivix t download.zip && echo OK",
    )
    story += command(
        "compress  (alias: z)",
        "Compress a single file (not an archive) using all cores.",
        "archivix compress <FILE> [-o OUT] [-c CODEC] [-l N] [--rm]",
        [
            ["`-c`, `--codec C`", "`zstd` (default), `gzip`, `xz`, `bzip2`, `lz4`, `brotli`, `lzma`."],
            ["`-o`, `--output PATH`", "Output path (default: input name plus extension)."],
            ["`--rm`", "Delete the input after success."],
        ],
        """
archivix z huge.log                  # -> huge.log.zst
archivix z huge.log -c gzip -l 9     # -> huge.log.gz
""",
    )
    story += command(
        "decompress  (alias: d)",
        "Decompress a single compressed file. The codec is detected automatically.",
        "archivix decompress <FILE> [-o OUT | -o -] [--rm]",
        [["`-o -`", "Write to standard output."], ["`--rm`", "Delete the compressed file after success."]],
        """
archivix d huge.log.zst              # -> huge.log
archivix d data.json.gz -o - | jq .
""",
    )
    story += command(
        "formats / mcp",
        "`formats` prints every supported format and what archivix can do with it (`--json` available). `mcp` starts the AI-agent server described in section 7.",
        "archivix formats [--json]\narchivix mcp",
        None,
        "archivix formats",
    )

    # 5. Patterns, levels, choosing
    story += H1("5. Patterns, levels and choosing a format")
    story += H2("Selecting files")
    story += [P("`extract`, `list` and `grep` accept member names in three forms:")]
    story += table(
        [
            ["You write", "It matches"],
            ["`docs/guide.md`", "exactly that file"],
            ["`docs`", "the folder and everything inside it"],
            ["`*.rs`", "every `.rs` file at any depth (a name-only glob)"],
            ["`src/**/*.rs`", "`.rs` files anywhere under `src/`"],
            ["`img/photo-?.png`", "`?` matches one character; `[abc]` and `{a,b}` also work"],
        ],
        [0.3, 0.7],
    )
    story += [P("Quote patterns (`'*.rs'`) so your shell does not expand them first.", "muted")]
    story += H2("Compression levels")
    story += table(
        [
            ["Codec", "Range", "Default", "Notes"],
            ["zstd (tar.zst, zip -m zstd)", "1–22", "3", "19+ is slow; negative levels (-7..-1) are even faster"],
            ["deflate (zip, tar.gz)", "0–9", "6", "0 = store only"],
            ["xz (tar.xz)", "0–9", "6", "highest ratio of the common formats"],
            ["7z (LZMA2)", "0–9", "6", ""],
            ["bzip2 (tar.bz2)", "1–9", "9", ""],
            ["brotli (tar.br)", "0–11", "6", ""],
            ["lz4 (tar.lz4)", "—", "—", "one fast level"],
        ],
        [0.32, 0.12, 0.12, 0.44],
    )
    story += H2("Which format should I use?")
    story += table(
        [
            ["If you want…", "Use"],
            ["the best all-round speed, instant listing and single-file reads", "`.tar.zst`"],
            ["something everyone can open (Windows, macOS Finder, phones)", "`.zip`"],
            ["maximum compatibility with Linux tooling", "`.tar.gz`"],
            ["the smallest possible file and time is not critical", "`.tar.xz` or `.7z`"],
            ["the fastest possible compression", "`.tar.lz4`"],
        ],
        [0.66, 0.34],
    )

    # 6. Desktop app
    story += H1("6. The desktop app")
    shot = DOCS / "screenshot-browse.png"
    if shot.exists():
        w = BODY_W
        img = Image(str(shot), width=w, height=w * 914 / 1400)
        story += [img, Paragraph("Browsing an archive, with a file selected and previewed on the right.", S["muted"]), Spacer(1, 8)]
    story += H2("Open and browse")
    story += bullets(
        [
            "Drag an archive onto the window, or click **Open Archive…**.",
            "Click a column header to sort; click again to reverse. Type in **Filter by name** to narrow the list.",
            "Click to select; **Cmd**-click (macOS) or **Ctrl**-click adds or removes items; **Shift**-click selects a range.",
            "Select a single file to **preview** it on the right: text as-is, anything else as a hex dump.",
        ]
    )
    story += H2("Extract and test")
    story += bullets(
        [
            "**Extract All…** or **Extract Selected (n)…** asks for a folder and extracts into a subfolder named after the archive.",
            "When it finishes, **Show in Finder / Explorer** opens the result.",
            "**Test** verifies every file's checksum without writing anything.",
        ]
    )
    story += H2("Create an archive")
    story += bullets(
        [
            "Drag files and folders onto the window (or use **New Archive**, then **Add Files…** / **Add Folder…**).",
            "Choose a **Format** (hover a choice for advice) and a **Level**.",
            "**Save as** shows where the archive will go; click **Change…** to pick another location.",
            "Keep **Skip system files** on to leave out `.DS_Store`, `Thumbs.db` and `__MACOSX`.",
            "Click **Create Archive**. The progress bar shows speed and file count; **Cancel** stops it.",
        ]
    )

    # 7. Agents
    story += H1("7. Using archivix from scripts and AI agents")
    story += H2("JSON output")
    story += [P("`list`, `info`, `test`, `grep` and `extract` accept `--json` and print one JSON document on standard output. The progress bar is never printed when output is not a terminal.")]
    story += [
        code(
            """
$ archivix ls --json project.zip 'README*'
{"count":1,"entries":[{"crc32":3473929125,"kind":"file","mode":420,
  "mtime":1794223761,"path":"README.md","size":4300, ...}],"format":"zip"}
"""
        )
    ]
    story += H2("MCP server")
    story += [P("`archivix mcp` speaks the Model Context Protocol over standard input/output, so assistants such as Claude can inspect archives without extracting them.")]
    story += [H3("Claude Code"), code("claude mcp add archivix -- archivix mcp")]
    story += [H3("Other MCP clients (JSON configuration)")]
    story += [
        code(
            """
{
  "mcpServers": {
    "archivix": { "command": "/usr/local/bin/archivix", "args": ["mcp"] }
  }
}
"""
        )
    ]
    story += table(
        [
            ["Tool", "What it does"],
            ["`archive_list`", "List members, with a glob filter and pagination."],
            ["`archive_info`", "Format, counts, sizes, ratio and capabilities."],
            ["`archive_read`", "Read one member (text, or a hex preview), paged by offset and size."],
            ["`archive_grep`", "Regex search across text members."],
            ["`archive_extract`", "Extract all or selected members to a folder."],
            ["`archive_create`", "Create an archive from paths."],
        ],
        [0.3, 0.7],
    )

    # 8. Troubleshooting
    story += H1("8. Troubleshooting")
    faq = [
        (
            "macOS: \"archivix can't be opened\" or \"cannot be verified\"",
            "The download is not notarized. Right-click the app and choose **Open**, or run `xattr -dr com.apple.quarantine /Applications/archivix.app`. For the command-line binary: `xattr -d com.apple.quarantine /usr/local/bin/archivix`.",
        ),
        ("Windows: SmartScreen blocks the app", "Click **More info**, then **Run anyway**. The binaries are not code-signed."),
        ("archivix: command not found", "The folder containing `archivix` is not on your PATH. See the installation steps for your platform, then open a new terminal."),
        (
            "\"cannot infer archive format\"",
            "The output name has no recognised extension. Name it e.g. `out.tar.zst`, or add `-F tar.zst`.",
        ),
        ("\"entry is encrypted: pass --password\"", "Add `-p 'your password'`. A wrong password reports \"provided password is incorrect\"."),
        ("I want to create a .rar file", "Not possible: RAR's compressor is proprietary. Use `.7z` or `.zip` instead."),
        (
            "Listing a .tar.gz from another tool is slower than one made by archivix",
            "Expected. Without the archivix index the whole stream must be decompressed to list it (archivix still does this faster than `tar`). Re-pack it with archivix to get instant listing.",
        ),
        ("No progress bar appears", "It only appears when output goes to a terminal and the operation takes longer than about 0.2 seconds; `-q` hides it."),
        (
            "Double-clicking an archive in Finder does not open it in the app",
            "Drag the archive onto the window or the Dock icon instead, or use **Open Archive…**.",
        ),
        ("Linux: the desktop app does not start", "It needs a graphical session (X11 or Wayland) and `libxkbcommon`, which desktop distributions include."),
    ]
    for q, a in faq:
        story.append(KeepTogether([Paragraph(f"<b>{esc(q)}</b>", S["body"]), P(a), Spacer(1, 3)]))
    story += [Spacer(1, 8), P(f"Report problems or ask questions at `{REPO}/issues`. Security issues: see SECURITY.md in the repository.", "muted")]

    doc = SimpleDocTemplate(
        str(OUT),
        pagesize=A4,
        leftMargin=MARGIN,
        rightMargin=MARGIN,
        topMargin=20 * mm,
        bottomMargin=18 * mm,
        title="archivix — Installation & User Guide",
        author="archivix contributors",
        subject=f"archivix {VERSION}",
    )
    doc.build(story, onFirstPage=cover_page, onLaterPages=later_pages)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    build()
