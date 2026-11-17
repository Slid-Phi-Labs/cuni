//! Julia backend — core-subset native seat over [`crate::codegen_core`].
//!
//! Emits plain Julia (runs under `julia script.jl` with no packages):
//! `println`, `function`/`end`, `if`/`end`, `while`/`end`, and `*` for
//! string concatenation.
//!
//! Semantic choices:
//! - Julia `/` on ints yields a float, so CuNi's truncating-toward-zero int
//!   division goes through the builtin `div` (`int_div_helper`), which
//!   truncates toward zero (verified: `div(-7, 2) == -3`).
//! - Julia `%` is `rem` (sign of the dividend), but the builtin `mod` is
//!   Python-floored (verified: `mod(-7, 2) == 1`, `mod(7, -2) == -1`), so
//!   `int_mod_helper` is just `mod`.
//! - `cuni_say(x, ty)` prints via `println`; the `ty` tag renders bools as
//!   `True`/`False` since Julia's native literals are `true`/`false`.
//! - The script body is wrapped in a bare `let ... end` block: at Julia's
//!   global scope a `while` loop cannot assign to an outer variable (hard
//!   scope), but inside `let` the loop sees the enclosing locals. `def`s
//!   stay top-level `function`s.
//! - `$` is escaped in string literals (Julia interpolates `$`).
//!
//! Honest caveat: Julia ints are `Int64`, so values are exact only within
//! ±2^63 (wraparound beyond); that is Julia's own arithmetic, not a CuNi lie.
//!
//! Refuses everything beyond the core subset (see codegen_core docs).

use crate::ast::Program;
use crate::codegen_core::{generate as core_generate, LangSpec};

fn jl_str(s: &str) -> String {
    let mut r = String::from('"');
    for c in s.chars() {
        match c {
            '"' => r.push_str("\\\""),
            '\\' => r.push_str("\\\\"),
            '\n' => r.push_str("\\n"),
            '\t' => r.push_str("\\t"),
            '\r' => r.push_str("\\r"),
            '$' => r.push_str("\\$"),
            c if (c as u32) < 0x20 => r.push_str(&format!("\\u{:04x}", c as u32)),
            c => r.push(c),
        }
    }
    r.push('"');
    r
}

const SPEC: LangSpec = LangSpec {
    comment: "#",
    header: "function cuni_say(x, ty)\n    if ty == \"bool\"\n        println(x ? \"True\" : \"False\")\n    else\n        println(x)\n    end\nend\nfunction cuni_dec_mul(a, b)\n    r = div(BigInt(a) * b, 10000)\n    if r > 9223372036854775807 || r < -9223372036854775808\n        throw(OverflowError(\"cuni: dec multiplication overflow\"))\n    end\n    Int64(r)\nend\nfunction cuni_dec_div(a, b)\n    if b == 0\n        throw(ErrorException(\"cuni: dec division by zero\"))\n    end\n    r = div(BigInt(a) * 10000, b)\n    if r > 9223372036854775807 || r < -9223372036854775808\n        throw(OverflowError(\"cuni: dec division overflow\"))\n    end\n    Int64(r)\nend\nfunction cuni_dec_str(v)\n    s = v < 0 ? \"-\" : \"\"\n    a = v < 0 ? (v == typemin(Int64) ? (UInt64(1) << 63) : UInt64(-v)) : UInt64(v)\n    ip = div(a, 10000)\n    fr = a % 10000\n    fs = lpad(string(fr), 4, \"0\")\n    while endswith(fs, \"0\")\n        fs = fs[1:end-1]\n    end\n    if isempty(fs)\n        fs = \"0\"\n    end\n    return s * string(ip) * \".\" * fs\nend\n",
    // NOTE (2026-11-17 fix): `checked_mul` is not defined in Main (Julia
    // 1.13; UndefVarError) and is over-strict anyway — it refuses a*b
    // overflowing Int64 even when a*b/10000 fits (e.g. max*10000). The
    // helpers now use BigInt intermediates with an explicit i64-envelope
    // check. dec_str takes the absolute value in UInt64 so typemin
    // formats instead of throwing (abs(typemin) overflows).
    main_open: "let",
    main_close: "end",
    let_fmt: "{name} = {expr}",
    mut_fmt: "{name} = {expr}",
    assign_fmt: "{name} = {expr}",
    fn_open_fmt: "function {name}({params})",
    fn_close: "end",
    ret_fmt: "return {expr}",
    if_open_fmt: "if {cond}",
    else_open: "else",
    block_close: "end",
    while_open_fmt: "while {cond}",
    say_fmt: "cuni_say({expr}, \"{ty}\")",
    call_fmt: "{name}({args})",
    true_lit: "true",
    false_lit: "false",
    str_quote: jl_str,
    // Julia `/` on ints yields a float; div() truncates toward zero (CuNi).
    int_div_helper: Some("div"),
    // Julia `%` is rem (sign of dividend); mod() is Python-floored (CuNi).
    int_mod_helper: Some("mod"),
    // dec (docs/DECIMAL.md §3, §6): Int64 seat; BigInt intermediates with
    // an explicit i64-envelope check refuse overflow loudly, div by zero
    // throws ErrorException.
    dec_mul_helper: Some("cuni_dec_mul"),
    dec_div_helper: Some("cuni_dec_div"),
    dec_str_helper: Some("cuni_dec_str"),
    dec_lit_suffix: "",
    dec_max_abs: Some(9_223_372_036_854_775_807),
    str_eq_op: "==",
    str_ne_op: "!=",
    str_lt_op: "<",
    str_gt_op: ">",
    str_le_op: "<=",
    str_ge_op: ">=",
    concat_fmt: "({l} * {r})",
    var_prefix: "",
    indent: "    ",
};

pub fn generate(program: &Program) -> Result<String, String> {
    core_generate(program, &SPEC)
}
