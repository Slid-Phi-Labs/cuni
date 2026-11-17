//! R backend — core-subset native seat over [`crate::codegen_core`].
//!
//! Emits plain R (runs under `Rscript` with no packages): `<-` bindings,
//! `name <- function(a, b) { ... return(expr) }`, `if (cond) { } else { }`,
//! `while (cond) { }`, and `paste0` for string concatenation.
//!
//! Semantic choices:
//! - R's `/` always yields a float, so CuNi's truncating-toward-zero int
//!   division goes through `cuni_div <- function(a, b) trunc(a / b)`
//!   (`int_div_helper`). The result is an integral-valued double, which is
//!   what R arithmetic works with.
//! - R's `%%` is already Python-floored (sign follows the divisor), so
//!   `cuni_mod <- function(a, b) a %% b` just names it (`int_mod_helper`);
//!   no core change needed.
//! - `cuni_say(x, ty)` prints via `cat(x, "\n", sep = "")` (the `sep = ""`
//!   matters: `cat`'s default separator is a space). The `ty` tag renders
//!   bools as `True`/`False` since R's native literals are `TRUE`/`FALSE`,
//!   and ints via `format(x, scientific = FALSE)` so large values never
//!   print as `1e+09`.
//!
//! Honest caveat: R numerics are doubles, so ints are exact only up to
//! 2^53. That bounds this seat's exactness range.
//!
//! Refuses everything beyond the core subset (see codegen_core docs).

use crate::ast::Program;
use crate::codegen_core::{generate as core_generate, LangSpec};

fn r_str(s: &str) -> String {
    let mut r = String::from('"');
    for c in s.chars() {
        match c {
            '"' => r.push_str("\\\""),
            '\\' => r.push_str("\\\\"),
            '\n' => r.push_str("\\n"),
            '\t' => r.push_str("\\t"),
            '\r' => r.push_str("\\r"),
            c if (c as u32) < 0x20 => r.push_str(&format!("\\u{:04x}", c as u32)),
            c => r.push(c),
        }
    }
    r.push('"');
    r
}

const SPEC: LangSpec = LangSpec {
    comment: "#",
    header: "# CuNi core-subset helpers (R)\n\
cuni_say <- function(x, ty) {\n\
  if (ty == \"bool\") { cat(if (x) \"True\" else \"False\", \"\\n\", sep = \"\") }\n\
  else if (ty == \"int\") { cat(format(x, scientific = FALSE, trim = TRUE), \"\\n\", sep = \"\") }\n\
  else { cat(x, \"\\n\", sep = \"\") }\n\
}\n\
cuni_div <- function(a, b) trunc(a / b)\n\
cuni_mod <- function(a, b) a %% b\n\
cuni_dec_mul <- function(a, b) trunc(a * b / 10000)\n\
cuni_dec_div <- function(a, b) { if (b == 0) stop(\"cuni: dec division by zero\"); trunc(a * 10000 / b) }\n\
cuni_dec_str <- function(v) {\n\
  s <- if (v < 0) \"-\" else \"\"\n\
  a <- abs(v)\n\
  ip <- trunc(a / 10000)\n\
  fr <- a %% 10000\n\
  fs <- sprintf(\"%04.0f\", fr)\n\
  fs <- sub(\"0+$\", \"\", fs)\n\
  if (fs == \"\") fs <- \"0\"\n\
  paste0(s, format(ip, scientific = FALSE, trim = TRUE), \".\", fs)\n\
}\n",
    main_open: "",
    main_close: "",
    let_fmt: "{name} <- {expr}",
    mut_fmt: "{name} <- {expr}",
    assign_fmt: "{name} <- {expr}",
    fn_open_fmt: "{name} <- function({params}) {",
    fn_close: "}",
    ret_fmt: "return({expr})",
    if_open_fmt: "if ({cond}) {",
    else_open: "} else {",
    block_close: "}",
    while_open_fmt: "while ({cond}) {",
    say_fmt: "cuni_say({expr}, '{ty}')",
    call_fmt: "{name}({args})",
    true_lit: "TRUE",
    false_lit: "FALSE",
    str_quote: r_str,
    // R `/` yields a float; trunc() rounds toward zero (CuNi `/`).
    int_div_helper: Some("cuni_div"),
    // R `%%` is already Python-floored; the helper just names it.
    int_mod_helper: Some("cuni_mod"),
    // dec (docs/DECIMAL.md §3, §6): doubles, so literals are
    // envelope-checked against 2^53 (exact-int range).
    dec_mul_helper: Some("cuni_dec_mul"),
    dec_div_helper: Some("cuni_dec_div"),
    dec_str_helper: Some("cuni_dec_str"),
    dec_lit_suffix: "",
    dec_max_abs: Some(9_007_199_254_740_992),
    str_eq_op: "==",
    str_ne_op: "!=",
    str_lt_op: "<",
    str_gt_op: ">",
    str_le_op: "<=",
    str_ge_op: ">=",
    concat_fmt: "paste0({l}, {r})",
    var_prefix: "",
    indent: "  ",
};

pub fn generate(program: &Program) -> Result<String, String> {
    core_generate(program, &SPEC)
}
