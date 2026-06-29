#!/usr/bin/env bash
set -uo pipefail
# ── Cross-Language Benchmark Runner ──────────────────────────
# All tests use identical parameters:
#   sum_1M:     add 0..999999
#   fn_call_1M: call add-one 1M times
#   map_1K:     map double over 1000 elements (1 pass)
#   fib_30:     recursive fibonacci(30)
# Usage: ./tools/bench.sh

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; NC='\033[0m'
BENCH_DIR="/tmp/zio-bench-$$"
mkdir -p "$BENCH_DIR"; trap "rm -rf '$BENCH_DIR'" EXIT

header() { echo -e "\n${CYAN}━━━ $1 ━━━${NC}"; }
result() { echo -e "  ${GREEN}$1${NC}: $2"; }
have() { command -v "$1" >/dev/null 2>&1; }

# ── 1. Rust ──────────────────────────────────────────────────────
have rustc && header "Rust"
have rustc && {
cat > "$BENCH_DIR/bench.rs" << 'RUSTEOF'
use std::time::Instant;
use std::hint::black_box;
fn main() {
    let start = Instant::now();
    let mut s: i64 = 0;
    for i in 0..1000000 { s = s.wrapping_add(black_box(i)); }
    let t = start.elapsed();
    println!("sum_1M:  {:.3}ms", t.as_secs_f64()*1000.0);

    fn a1(x: i64) -> i64 { x.wrapping_add(1) }
    let start = Instant::now();
    let mut v: i64 = 0;
    for _ in 0..1000000 { v = a1(black_box(v)); }
    let t = start.elapsed();
    println!("fn_call_1M: {:.3}ms", t.as_secs_f64()*1000.0);

    let seq: Vec<i64> = (0..1000).collect();
    let start = Instant::now();
    let _: Vec<i64> = seq.iter().map(|x| black_box(x)*2).collect();
    let t = start.elapsed();
    println!("map_1K: {:.3}ms", t.as_secs_f64()*1000.0);

    fn fib(x: u32) -> u64 { match black_box(x) { 0=>0,1=>1,_=>fib(x-1)+fib(x-2) } }
    let start = Instant::now();
    let r = fib(30);
    let t = start.elapsed();
    println!("fib_30: {:.3}ms", t.as_secs_f64()*1000.0);
}
RUSTEOF
    rustc -O "$BENCH_DIR/bench.rs" -o "$BENCH_DIR/bench_rust" 2>/dev/null
    "$BENCH_DIR/bench_rust" | while read l; do result "Rust" "$l"; done
}

# ── 2. Python ────────────────────────────────────────────────────
have python3 && header "Python"
have python3 && python3 -c '
import time
t1=time.perf_counter(); s=0
for i in range(1000000): s+=i
print("sum_1M:  %.3fms" % ((time.perf_counter()-t1)*1000))

def a1(x): return x+1
t1=time.perf_counter(); v=0
for _ in range(1000000): v=a1(v)
print("fn_call_1M: %.3fms" % ((time.perf_counter()-t1)*1000))

seq=list(range(1000))
t1=time.perf_counter()
m=[x*2 for x in seq]
print("map_1K: %.3fms" % ((time.perf_counter()-t1)*1000))

def fib(x): return x if x<2 else fib(x-1)+fib(x-2)
t1=time.perf_counter(); r=fib(30)
print("fib_30: %.3fms" % ((time.perf_counter()-t1)*1000))
' | while read l; do result "Python" "$l"; done

# ── 3. Node.js ────────────────────────────────────────────────────
have node && header "Node.js"
have node && node -e '
let s=0; let t1=performance.now();
for(let i=0;i<1000000;i++) s+=i;
console.log("sum_1M:  "+(performance.now()-t1).toFixed(3)+"ms");

function a1(x){return x+1}
let v=0; t1=performance.now();
for(let i=0;i<1000000;i++) v=a1(v);
console.log("fn_call_1M: "+(performance.now()-t1).toFixed(3)+"ms");

const seq=Array.from({length:1000},(_,i)=>i);
t1=performance.now();
const m=seq.map(x=>x*2);
console.log("map_1K: "+(performance.now()-t1).toFixed(3)+"ms");

function fib(x){return x<2?x:fib(x-1)+fib(x-2)}
t1=performance.now(); const r=fib(30);
console.log("fib_30: "+(performance.now()-t1).toFixed(3)+"ms");
' | while read l; do result "Node" "$l"; done

# ── 4. SBCL Common Lisp ──────────────────────────────────────────
have sbcl && header "SBCL"
have sbcl && {
cat > "$BENCH_DIR/bench.lisp" << 'CLEOF'
(declaim (optimize (speed 3) (safety 0)))
(defun run ()
  (flet ((ms (fn)
           (let ((t1 (get-internal-real-time))) (funcall fn)
             (* (/ (- (get-internal-real-time) t1) internal-time-units-per-second 1.0) 1000))))
    (let ((s 0)) (declare (fixnum s))
      (format t "sum_1M:  ~,3fms~%" (ms #'(lambda() (dotimes (i 1000000) (incf s i))))))
    (flet ((a1 (x) (1+ x)))
      (let ((v 0)) (declare (fixnum v))
        (format t "fn_call_1M: ~,3fms~%" (ms #'(lambda() (dotimes (i 1000000) (setf v (a1 v))))))))
    (let ((seq (loop for i below 1000 collect i)))
      (format t "map_1K: ~,3fms~%" (ms #'(lambda() (loop for x in seq collect (* x 2))))))
    (format t "fib_30: ~,3fms~%" (ms #'(lambda()
      (labels ((fib (x) (if (< x 2) x (+ (fib (- x 1)) (fib (- x 2)))))) (fib 30)))))))
(run) (sb-ext:quit)
CLEOF
    sbcl --script "$BENCH_DIR/bench.lisp" 2>/dev/null | while read l; do result "SBCL" "$l"; done
}

# ── 5. Zio ──────────────────────────────────────────────────────────
header "Zio"
ZIO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ZIO_BIN="$ZIO_DIR/target/release/zio-cli"
(cd "$ZIO_DIR" && cargo build --release 2>/dev/null >/dev/null)

run_zio() {
    local name="$1" code="$2"
    printf '%s\n' "$code" > "$BENCH_DIR/bench_$name.zio"
    local t1=$(date +%s%N)
    "$ZIO_BIN" "$BENCH_DIR/bench_$name.zio" >/dev/null 2>/dev/null || true
    local t2=$(date +%s%N)
    echo "$(( (t2 - t1) / 1000000 ))"
}

result "Zio" "sum_1M:  $(run_zio sum_1M '(loop [i 0 s 0] (if (< i 1000000) (recur (+ i 1) (+ s i)) s))')ms"
result "Zio" "fn_call_1M: $(run_zio fn_call_1M \
  "(defn a1 [x] (+ x 1))(loop [i 0 v 0] (if (< i 1000000) (recur (+ i 1) (a1 v)) v))")ms"
result "Zio" "map_1K:  $(run_zio map_1K \
  "(def seq (range 1000))(defn dbl [x] (* x 2))(count (map dbl seq))")ms"
result "Zio" "fib_30: $(run_zio fib_30 \
  "(defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))(fib 30)")ms"

echo -e "\n${CYAN}━━━ Done ━━━${NC}"
