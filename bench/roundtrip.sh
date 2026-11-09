#!/bin/bash
# usage: roundtrip.sh <ax-binary> <corpus-dir> <workdir>
AX=$1; C=$2; W=$3; mkdir -p $W
cd $C/..
CN=$(basename $C)
pass=0; fail=0
for ext in zip tar tar.gz tar.zst tar.xz tar.bz2 tar.lz4 tar.br 7z; do
  A=$W/t.$ext; O=$W/out_$ext
  mkdir -p $O
  if ! $AX -q create $A $CN 2>$W/err_$ext; then echo "FAIL create $ext: $(cat $W/err_$ext)"; fail=$((fail+1)); continue; fi
  n=$($AX list $A | wc -l | tr -d ' ')
  $AX -q test $A 2>>$W/err_$ext || { echo "FAIL test $ext: $(tail -3 $W/err_$ext)"; fail=$((fail+1)); }
  if ! $AX -q extract $A -C $O 2>>$W/err_$ext; then echo "FAIL extract $ext: $(tail -3 $W/err_$ext)"; fail=$((fail+1)); continue; fi
  if diff -r --no-dereference $C $O/$CN >$W/diff_$ext 2>&1; then
    echo "ok   $ext  entries=$n size=$(stat -f %z $A)"; pass=$((pass+1))
  else echo "FAIL diff $ext: $(head -5 $W/diff_$ext)"; fail=$((fail+1)); fi
done
echo "--- reference tools on archivix output"
unzip -tq $W/t.zip >/dev/null && echo "ok   unzip -t" || echo "FAIL unzip -t"
for ext in tar tar.gz tar.zst tar.xz tar.bz2 tar.lz4; do
  case $ext in tar) cmd="cat";; tar.gz) cmd="gzip -dc";; tar.zst) cmd="zstd -dcq";; tar.xz) cmd="xz -dc";; tar.bz2) cmd="bzip2 -dc";; tar.lz4) cmd="lz4 -dcq";; esac
  M=$W/ref_$ext; mkdir -p $M
  if $cmd $W/t.$ext | tar -xf - -C $M 2>$W/referr_$ext && diff -r --no-dereference $C $M/$CN >/dev/null 2>&1; then echo "ok   $cmd | tar -x  ($ext)"; else echo "FAIL $cmd | tar ($ext): $(head -3 $W/referr_$ext)"; fi
done
brotli -dc $W/t.tar.br | tar -tf - >/dev/null 2>&1 && echo "ok   brotli -d | tar -t" || echo "FAIL brotli"
echo "pass=$pass fail=$fail"
