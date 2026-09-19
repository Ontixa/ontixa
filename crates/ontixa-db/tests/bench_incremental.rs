//! Incremental-compile benchmarks — run manually:
//!
//! ```text
//! cargo test -p ontixa-db --test bench_incremental -- --ignored --nocapture
//! ```
//!
//! Prints a markdown table of wall-clock medians (cold compile,
//! no-op recompile, single-body edit, signature edit). These are
//! `dev`-profile interpreter-pipeline numbers — see
//! `docs/benchmark-philosophy.md` before quoting them.

use ontixa_db::Db;
use std::time::Instant;

/// A `data` type plus `n` functions in a dependency chain:
/// `f_i` calls `f_{i+1}`; the last one returns a constant.
fn chain_src(n: usize) -> String {
    let mut s = String::from("data P { x: i32; }\n");
    for i in 0..n {
        if i + 1 == n {
            s.push_str(&format!("fn f{i}(p: i32) -> i32 {{ return p + 1; }}\n"));
        } else {
            let j = i + 1;
            s.push_str(&format!(
                "fn f{i}(p: i32) -> i32 {{ return f{j}(p) + 1; }}\n"
            ));
        }
    }
    s.push_str("fn main() -> i32 { return f0(0); }");
    s
}

/// `n` independent functions — the isolating case for per-def queries.
fn wide_src(n: usize) -> String {
    let mut s = String::from("data P { x: i32; }\n");
    for i in 0..n {
        s.push_str(&format!("fn g{i}(p: i32) -> i32 {{ return p + {i}; }}\n"));
    }
    s.push_str("fn main() -> i32 { return g0(0); }");
    s
}

fn median_ms(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn timed(db: &mut Db, f: usize) -> (f64, usize) {
    let t0 = Instant::now();
    assert!(db.compile(f).is_valid());
    (t0.elapsed().as_secs_f64() * 1e3, db.last_evaluated().len())
}

/// One benchmark row: cold compile, then `iters` rounds of
/// no-op recompile / body-edit / revert.
fn bench_row(name: &str, src: &str, edited: &str, iters: usize) {
    let mut db = Db::new();
    let f = db.add_source(src);
    let (cold_ms, cold_evals) = timed(&mut db, f);

    let mut noop = Vec::new();
    let mut edit = Vec::new();
    let mut edit_evals = Vec::new();
    for _ in 0..iters {
        let (ms, evals) = timed(&mut db, f);
        assert_eq!(evals, 0, "no-op recompile must evaluate nothing");
        noop.push(ms);
        db.set_source(f, edited);
        let (ms, n) = timed(&mut db, f);
        edit.push(ms);
        edit_evals.push(n);
        if edit.len() == 1 {
            // Where does an edit-compile spend its time? File-level
            // stages re-run whole-file work; per-def stages show the
            // incremental saving.
            for t in &db.compile(f).timings {
                println!(
                    "    {name} edit: {:<10} {:>8.2} ms",
                    t.stage,
                    t.nanos as f64 / 1e6
                );
            }
        }
        db.set_source(f, src);
        timed(&mut db, f);
    }
    println!(
        "| {name} | {cold_ms:.2} | {} | {:.3} | {:.3} | {} |",
        cold_evals,
        median_ms(noop),
        median_ms(edit),
        edit_evals[0],
    );
}

#[test]
#[ignore = "benchmark — run with --ignored --nocapture"]
fn incremental_compile_bench() {
    let iters = 20;
    println!(
        "\n| workload | cold ms | cold evals | no-op ms (med) | body-edit ms (med) | body-edit evals |"
    );
    println!("|---|---|---|---|---|---|");

    // `return p + 1;` appears only in each chain's tail fn; the edit
    // is length-preserving (`1`→`2`), so it cannot shift later items'
    // spans — a clean single-body-edit measurement.
    let c64 = chain_src(64);
    bench_row(
        "chain-64",
        &c64,
        &c64.replace("return p + 1; }", "return p + 2; }"),
        iters,
    );
    let c256 = chain_src(256);
    bench_row(
        "chain-256",
        &c256,
        &c256.replace("return p + 1; }", "return p + 2; }"),
        iters,
    );
    let w64 = wide_src(64);
    bench_row(
        "wide-64",
        &w64,
        &w64.replacen("return p + 0;", "return p + 9;", 1),
        iters,
    );
    let w256 = wide_src(256);
    bench_row(
        "wide-256",
        &w256,
        &w256.replacen("return p + 0;", "return p + 9;", 1),
        iters,
    );
}
