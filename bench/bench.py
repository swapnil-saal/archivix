#!/usr/bin/env python3
"""archivix benchmark harness: wall-clock medians vs installed reference tools."""
import json, os, shutil, statistics, subprocess, sys, time

B = sys.argv[1]            # bench root (contains corp/)
AX = sys.argv[2]           # archivix binary
GROUPS = sys.argv[3].split(",") if len(sys.argv) > 3 else None
C = os.path.join(B, "corp")
A = os.path.join(B, "arch"); os.makedirs(A, exist_ok=True)
T = os.path.join(B, "tmp")
results = []

def sh(cmd):
    r = subprocess.run(cmd, shell=True, cwd=C, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if r.returncode not in (0,):
        raise RuntimeError(f"{cmd}\n{r.stderr.decode()[-400:]}")

def timed(cmd, out_dir=False):
    def once():
        if out_dir:
            shutil.rmtree(T, ignore_errors=True); os.makedirs(T)
        t = time.perf_counter(); sh(cmd.replace("{T}", T)); return time.perf_counter() - t
    times = [once()]
    if times[0] < 10:
        times += [once(), once()]
    if out_dir:
        shutil.rmtree(T, ignore_errors=True)
    return statistics.median(times)

def case(group, scenario, tool, cmd, out=None, out_dir=False, input_bytes=None):
    if GROUPS and group not in GROUPS:
        return
    if out and os.path.exists(out):
        os.remove(out)
    try:
        t = timed(cmd, out_dir)
    except Exception as e:
        print(f"  ERROR {tool}: {e}", flush=True); return
    size = os.path.getsize(out) if out and os.path.exists(out) else None
    row = dict(group=group, scenario=scenario, tool=tool, seconds=round(t, 4), size=size, input_bytes=input_bytes)
    results.append(row)
    s = f"{size/1e6:9.1f} MB" if size else " " * 12
    print(f"  {scenario:<34} {tool:<34} {t:8.3f}s {s}", flush=True)

def du(p):
    total = 0
    for root, _, files in os.walk(os.path.join(C, p)):
        for f in files:
            fp = os.path.join(root, f)
            if not os.path.islink(fp):
                total += os.path.getsize(fp)
    return total

src_b, mix_b, big_b = du("src"), du("mixed"), os.path.getsize(os.path.join(C, "big.bin"))
a = lambda n: os.path.join(A, n)

for corp, nbytes in (("src", src_b), ("mixed", mix_b)):
    g = f"zip-{corp}"
    print(f"== {g}", flush=True)
    case(g, f"create zip ({corp})", "zip -r (Info-ZIP)", f"zip -qry {a(corp+'.info.zip')} {corp}", a(corp+".info.zip"), input_bytes=nbytes)
    case(g, f"create zip ({corp})", "bsdtar --format zip", f"tar --format zip -cf {a(corp+'.bsd.zip')} {corp}", a(corp+".bsd.zip"), input_bytes=nbytes)
    case(g, f"create zip ({corp})", "archivix", f"{AX} -q c {a(corp+'.ax.zip')} {corp}", a(corp+".ax.zip"), input_bytes=nbytes)
    if os.path.exists(a(corp+".info.zip")):
        case(g, f"extract zip ({corp})", "unzip", f"unzip -q {a(corp+'.info.zip')} -d {{T}}", out_dir=True, input_bytes=nbytes)
        case(g, f"extract zip ({corp})", "bsdtar -x", f"tar -xf {a(corp+'.info.zip')} -C {{T}}", out_dir=True, input_bytes=nbytes)
        case(g, f"extract zip ({corp})", "archivix", f"{AX} -q x {a(corp+'.info.zip')} -C {{T}}", out_dir=True, input_bytes=nbytes)
        case(g, f"list zip ({corp})", "unzip -l", f"unzip -l {a(corp+'.info.zip')}")
        case(g, f"list zip ({corp})", "archivix ls", f"{AX} ls {a(corp+'.info.zip')}")

for corp, nbytes in (("src", src_b), ("mixed", mix_b)):
    g = f"tgz-{corp}"
    print(f"== {g}", flush=True)
    case(g, f"create tar.gz ({corp})", "tar -czf (bsdtar)", f"tar -czf {a(corp+'.bsd.tgz')} {corp}", a(corp+".bsd.tgz"), input_bytes=nbytes)
    case(g, f"create tar.gz ({corp})", "tar | gzip -6", f"tar -cf - {corp} | gzip -6 > {a(corp+'.gz.tgz')}", a(corp+".gz.tgz"), input_bytes=nbytes)
    case(g, f"create tar.gz ({corp})", "archivix", f"{AX} -q c {a(corp+'.ax.tar.gz')} {corp}", a(corp+".ax.tar.gz"), input_bytes=nbytes)
    case(g, f"extract tar.gz ({corp})", "tar -xzf  [std .tgz]", f"tar -xzf {a(corp+'.bsd.tgz')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"extract tar.gz ({corp})", "archivix [std .tgz]", f"{AX} -q x {a(corp+'.bsd.tgz')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"extract tar.gz ({corp})", "tar -xzf  [archivix .tgz]", f"tar -xzf {a(corp+'.ax.tar.gz')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"extract tar.gz ({corp})", "archivix [archivix .tgz]", f"{AX} -q x {a(corp+'.ax.tar.gz')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"list tar.gz ({corp})", "tar -tzf", f"tar -tzf {a(corp+'.bsd.tgz')}")
    case(g, f"list tar.gz ({corp})", "archivix ls [std .tgz]", f"{AX} ls {a(corp+'.bsd.tgz')}")
    case(g, f"list tar.gz ({corp})", "archivix ls [archivix .tgz]", f"{AX} ls {a(corp+'.ax.tar.gz')}")

g = "zst"
print(f"== {g}", flush=True)
for corp, nbytes in (("src", src_b), ("mixed", mix_b)):
    case(g, f"create tar.zst ({corp})", "tar | zstd -T0 -3", f"tar -cf - {corp} | zstd -f -q -T0 -3 -o {a(corp+'.cli.tar.zst')}", a(corp+".cli.tar.zst"), input_bytes=nbytes)
    case(g, f"create tar.zst ({corp})", "archivix", f"{AX} -q c {a(corp+'.ax.tar.zst')} {corp}", a(corp+".ax.tar.zst"), input_bytes=nbytes)
    case(g, f"extract tar.zst ({corp})", "zstd -dc | tar -x", f"zstd -dcq {a(corp+'.cli.tar.zst')} | tar -xf - -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"extract tar.zst ({corp})", "archivix [zstd-cli file]", f"{AX} -q x {a(corp+'.cli.tar.zst')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"extract tar.zst ({corp})", "archivix [archivix file]", f"{AX} -q x {a(corp+'.ax.tar.zst')} -C {{T}}", out_dir=True, input_bytes=nbytes)
    case(g, f"list tar.zst ({corp})", "zstd -dc | tar -t", f"zstd -dcq {a(corp+'.cli.tar.zst')} | tar -tf -")
    case(g, f"list tar.zst ({corp})", "archivix ls [archivix file]", f"{AX} ls {a(corp+'.ax.tar.zst')}")

g = "member"
print(f"== {g}", flush=True)
member = "src/zip-9.5.0/src/read/zip_archive.rs"
case(g, "read 1 member (tar.gz, src)", "tar -xOzf", f"tar -xOzf {a('src.bsd.tgz')} {member}")
case(g, "read 1 member (tar.gz, src)", "archivix cat [archivix .tgz]", f"{AX} cat {a('src.ax.tar.gz')} {member}")
case(g, "read 1 member (tar.zst, src)", "zstd -dc | tar -xO", f"zstd -dcq {a('src.cli.tar.zst')} | tar -xOf - {member}")
case(g, "read 1 member (tar.zst, src)", "archivix cat [archivix file]", f"{AX} cat {a('src.ax.tar.zst')} {member}")
case(g, "read 1 member (zip, src)", "unzip -p", f"unzip -p {a('src.info.zip')} {member}")
case(g, "read 1 member (zip, src)", "archivix cat", f"{AX} cat {a('src.info.zip')} {member}")

g = "xz"
print(f"== {g}", flush=True)
case(g, "create tar.xz (src)", "tar | xz -T0 -6", f"tar -cf - src | xz -T0 -6 > {a('src.cli.tar.xz')}", a("src.cli.tar.xz"), input_bytes=src_b)
case(g, "create tar.xz (src)", "archivix", f"{AX} -q c {a('src.ax.tar.xz')} src", a("src.ax.tar.xz"), input_bytes=src_b)
case(g, "extract tar.xz (src)", "xz -dc | tar -x", f"xz -dc {a('src.cli.tar.xz')} | tar -xf - -C {{T}}", out_dir=True, input_bytes=src_b)
case(g, "extract tar.xz (src)", "archivix", f"{AX} -q x {a('src.cli.tar.xz')} -C {{T}}", out_dir=True, input_bytes=src_b)

g = "bz2"
print(f"== {g}", flush=True)
case(g, "create tar.bz2 (src)", "tar -cjf (bsdtar)", f"tar -cjf {a('src.bsd.tbz')} src", a("src.bsd.tbz"), input_bytes=src_b)
case(g, "create tar.bz2 (src)", "archivix", f"{AX} -q c {a('src.ax.tar.bz2')} src", a("src.ax.tar.bz2"), input_bytes=src_b)
case(g, "extract tar.bz2 (src)", "tar -xjf  [std]", f"tar -xjf {a('src.bsd.tbz')} -C {{T}}", out_dir=True, input_bytes=src_b)
case(g, "extract tar.bz2 (src)", "archivix [archivix file]", f"{AX} -q x {a('src.ax.tar.bz2')} -C {{T}}", out_dir=True, input_bytes=src_b)

g = "7z"
print(f"== {g}", flush=True)
case(g, "create 7z (src)", "bsdtar --format 7zip", f"tar --format 7zip -cf {a('src.bsd.7z')} src", a("src.bsd.7z"), input_bytes=src_b)
case(g, "create 7z (src)", "archivix", f"{AX} -q c {a('src.ax.7z')} src", a("src.ax.7z"), input_bytes=src_b)
case(g, "extract 7z (src)", "bsdtar -x", f"tar -xf {a('src.bsd.7z')} -C {{T}}", out_dir=True, input_bytes=src_b)
case(g, "extract 7z (src)", "archivix", f"{AX} -q x {a('src.bsd.7z')} -C {{T}}", out_dir=True, input_bytes=src_b)

g = "single"
print(f"== {g}", flush=True)
big = os.path.join(C, "big.bin")
for codec, ref, refd in (
    ("gzip", f"gzip -6 -c {big} > {a('big.gz')}", f"gzip -dc {a('big.gz')} > /dev/null"),
    ("zstd", f"zstd -q -T0 -3 -c {big} > {a('big.zst')}", f"zstd -dcq {a('big.zst')} > /dev/null"),
    ("lz4", f"lz4 -q -c {big} > {a('big.lz4')}", f"lz4 -dcq {a('big.lz4')} > /dev/null"),
    ("xz", f"xz -T0 -6 -c {big} > {a('big.xz')}", f"xz -dc {a('big.xz')} > /dev/null"),
    ("bzip2", f"bzip2 -9 -c {big} > {a('big.bz2')}", f"bzip2 -dc {a('big.bz2')} > /dev/null"),
):
    ext = {"gzip": "gz", "zstd": "zst", "lz4": "lz4", "xz": "xz", "bzip2": "bz2"}[codec]
    refname = {"gzip": "gzip -6", "zstd": "zstd -T0 -3", "lz4": "lz4", "xz": "xz -T0 -6", "bzip2": "bzip2 -9"}[codec]
    case(g, f"compress 497MB file ({codec})", refname, ref, a(f"big.{ext}"), input_bytes=big_b)
    case(g, f"compress 497MB file ({codec})", "archivix", f"{AX} -q compress -c {codec} {big} -o {a('big.ax.'+ext)}", a(f"big.ax.{ext}"), input_bytes=big_b)
    case(g, f"decompress 497MB file ({codec})", refname.split()[0] + " -d", refd, input_bytes=big_b)
    case(g, f"decompress 497MB file ({codec})", "archivix [archivix file]", f"{AX} -q d {a('big.ax.'+ext)} -o /dev/null", input_bytes=big_b)

out = os.path.join(B, "results-" + ("-".join(GROUPS) if GROUPS else "all") + ".json")
json.dump(results, open(out, "w"), indent=1)
print("saved", out)
