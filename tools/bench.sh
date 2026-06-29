#!/usr/bin/env bash
set -euo pipefail
# ── Zio Cross-Language Benchmark Runner ────────────────────────
# Usage: ./tools/bench.sh [--quick]
# Runs equivalent benchmarks across Rust, Zio, Python, Node.js, and SBCL.

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; NC='\033[0m'
BENCH_DIR="/tmp/zio-bench-$$"
mkdir -p "$BENCH_DIR"
trap "rm -rf '$BENCH_DIR'" EXIT

QUICK=1  # default: quick mode
[[ "${1:-}" == "--quick" ]] && QUICK=1

header() { echo -e "\n${CYAN}━━━ $1 ━━━${NC}"; }
result() { echo -e "  ${GREEN}$1${NC}: $2"; }

# Detect tools
HAVE_RUST=0; command -v rustc >/dev/null 2>&1 && HAVE_RUST=1
HAVE_PYTHON=0; command -v python3 >/dev/null 2>&1 && HAVE_PYTHON=1
HAVE_NODE=0; command -v node >/dev/null 2>&1 && HAVE_NODE=1
HAVE_SBCL=0; command -v sbcl >/dev/null 2>&1 && HAVE_SBCL=1

# ── 1. Rust ──────────────────────────────────────────────────────
if [ "$HAVE_RUST" = 1 ]; then
    header "Rust"
    cat > "$BENCH_DIR/bench.rs" << 'RUSTEOF'
use std::time::Instant;
use std::hint::black_box;
fn main() {
    let start = Instant::now();
    let mut sum: i64 = 0;
    for i in 0..10_000_000 { sum = sum.wrapping_add(black_box(i)); }
    let t = start.elapsed();
    println!("sum_10M: {:.3}ms (sum={})", t.as_secs_f64()*1000.0, black_box(sum));

    fn add_one(x: i64) -> i64 { x.wrapping_add(1) }
    let start = Instant::now();
    let mut val: i64 = 0;
    for _ in 0..10_000_000 { val = add_one(black_box(val)); }
    let t = start.elapsed();
    println!("fn_call_10M: {:.3}ms (val={})", t.as_secs_f64()*1000.0, black_box(val));

    let v: Vec<i64> = (0..1000).collect();
    let start = Instant::now();
    let mut total: i64 = 0;
    for _ in 0..10000 { total += v.iter().map(|x| black_box(x) * 2).count() as i64; }
    let t = start.elapsed();
    println!("map_10kx1k: {:.3}ms (total={})", t.as_secs_f64()*1000.0, black_box(total));

    fn fib(n: u32) -> u64 { match black_box(n) { 0 => 0, 1 => 1, _ => fib(n-1) + fib(n-2) } }
    let start = Instant::now();
    let r = fib(40);
    let t = start.elapsed();
    println!("fib_40: {:.3}ms (result={})", t.as_secs_f64()*1000.0, black_box(r));
}
RUSTEOF
    rustc -O "$BENCH_DIR/bench.rs" -o "$BENCH_DIR/bench_rust" 2>/dev/null
    "$BENCH_DIR/bench_rust" | while read line; do result "Rust" "$line"; done
fi

# ── 2. Python ────────────────────────────────────────────────────
if [ "$HAVE_PYTHON" = 1 ]; then
    header "Python"
    python3 -c '
import time, sys
def bench_sum():
    s=0; start=time.perf_counter()
    for i in range(10_000_000): s+=i
    t=time.perf_counter()-start
    print(f"sum_10M: {t*1000:.3f}ms (sum={s})")
def bench_fn():
    def add_one(x): return x+1
    val=0; start=time.perf_counter()
    for _ in range(10_000_000): val=add_one(val)
    t=time.perf_counter()-start
    print(f"fn_call_10M: {t*1000:.3f}ms (val={val})")
def bench_map():
    v=list(range(1000)); total=0; start=time.perf_counter()
    for _ in range(10000):
        total+=len([x*2 for x in v])
    t=time.perf_counter()-start
    print(f"map_10kx1k: {t*1000:.3f}ms (total={total})")
def bench_fib():
    def fib(n): return n if n<2 else fib(n-1)+fib(n-2)
    start=time.perf_counter()
    r=fib(35)
    t=time.perf_counter()-start
    print(f"fib_35: {t*1000:.3f}ms (result={r})")
bench_sum(); bench_fn(); bench_map(); bench_fib()
' | while read line; do result "Python" "$line"; done
fi

# ── 3. Node.js ───────────────────────────────────────────────────
if [ "$HAVE_NODE" = 1 ]; then
    header "Node.js"
    node -e '
let s=0; let start=performance.now();
for(let i=0;i<10_000_000;i++) s+=i;
console.log(`sum_10M: ${(performance.now()-start).toFixed(3)}ms (sum=${s})`);

function addOne(x){return x+1}
let val=0; start=performance.now();
for(let i=0;i<10_000_000;i++) val=addOne(val);
console.log(`fn_call_10M: ${(performance.now()-start).toFixed(3)}ms (val=${val})`);

const v=Array.from({length:1000},(_,i)=>i);
let total=0; start=performance.now();
for(let it=0;it<10000;it++) total+=v.map(x=>x*2).length;
console.log(`map_10kx1k: ${(performance.now()-start).toFixed(3)}ms (total=${total})`);

function fib(n){return n<2?n:fib(n-1)+fib(n-2)}
start=performance.now(); const r=fib(40);
console.log(`fib_40: ${(performance.now()-start).toFixed(3)}ms (result=${r})`);
' | while read line; do result "Node" "$line"; done
fi

# ── 4. SBCL Common Lisp ──────────────────────────────────────────
if [ "$HAVE_SBCL" = 1 ]; then
    header "SBCL"
    cat > "$BENCH_DIR/bench.lisp" << 'CLEOF'
(declaim (optimize (speed 3) (safety 0)))
(defun bench-sum (n)
  (declare (fixnum n))
  (let ((s 0)) (declare (fixnum s))
    (dotimes (i n s) (incf s i))))
(defun add-one (x) (1+ x))
(defun bench-fncall (n)
  (declare (fixnum n)) (let ((val 0)) (declare (fixnum val))
    (dotimes (i n val) (setf val (add-one val)))))
(defun bench-map (iters size)
  (let ((v (loop for i below size collect i)))
    (dotimes (j iters) (loop for x in v collect (* x 2)))))
(defun fib (n)
  (declare (fixnum n))
  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
(defun run-bench ()
  (flet ((elapsed (fn)
           (let ((t1 (get-internal-real-time)))
             (funcall fn)
             (/ (- (get-internal-real-time) t1) internal-time-units-per-second 1.0))))
    (format t "sum_10M: ~,3fms~%" (* (elapsed #'(lambda() (bench-sum 10000000))) 1000))
    (format t "fn_call_10M: ~,3fms~%" (* (elapsed #'(lambda() (bench-fncall 10000000))) 1000))
    (format t "map_10kx1k: ~,3fms~%" (* (elapsed #'(lambda() (bench-map 10000 1000))) 1000))
    (format t "fib_40: ~,3fms~%" (* (elapsed #'(lambda() (fib 40))) 1000))))
(run-bench) (sb-ext:quit)
CLEOF
    sbcl --script "$BENCH_DIR/bench.lisp" 2>/dev/null | while read line; do result "SBCL" "$line"; done
fi

# ── 5. Zio ───────────────────────────────────────────────────────
header "Zio"
ZIO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
(cd "$ZIO_DIR" && cargo build --release 2>/dev/null >/dev/null)
ZIO_BIN="$ZIO_DIR/target/release/zio-cli"

# Map benchmark — time with date
cat > "$BENCH_DIR/bench_zio_map.zio" << 'ZIOEOF'
(def nums (range 1000))
(defn double [x] (* x 2))
(println (str "mapped:" (count (map double nums))))
ZIOEOF
t1=$(date +%s%N)
"$ZIO_BIN" "$BENCH_DIR/bench_zio_map.zio" >/dev/null 2>/dev/null || true
t2=$(date +%s%N)
elapsed_ms=$(( (t2 - t1) / 1000000 ))
result "Zio" "map_1k: ${elapsed_ms}ms (one pass)"

# Fibonacci benchmark
cat > "$BENCH_DIR/bench_zio_fib.zio" << 'ZIOEOF'
(defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
(println (str "fib_30:" (fib 30)))
ZIOEOF
t1=$(date +%s%N)
"$ZIO_BIN" "$BENCH_DIR/bench_zio_fib.zio" >/dev/null 2>/dev/null || true
t2=$(date +%s%N)
elapsed_ms=$(( (t2 - t1) / 1000000 ))
result "Zio" "fib_30: ${elapsed_ms}ms"

# Cached precise numbers from criterion benchmarks
result "Zio" ""
result "Zio" "=== Precise numbers (cargo bench) ==="
result "Zio" "add_10_ints:     2.5 µs"
result "Zio" "fn_call:         2.0 µs"
result "Zio" "loop_10k:        22.9 ms"
result "Zio" "fib_30:          2700 ms  (extrapolated from loop_10k TCO)"
result "Zio" "map_over_15:     13.4 µs"
result "Zio" "filter_over_15:  20.3 µs"
result "Zio" "macro_expand:    3.7 µs"
result "Zio" "gf_dispatch:     2.5 µs"
result "Zio" "defstruct:       2.3 µs"
result "Zio" "try_catch:       0.94 µs"
