use bumpalo::Bump;
use std::path::{Path, PathBuf};
use std::str::from_utf8_unchecked;
use target_lexicon::Triple;

use roc_can::builtins::builtin_defs_map;
use roc_collections::all::{MutMap, MutSet};
use roc_fmt::annotation::Formattable;
use roc_fmt::annotation::{Newlines, Parens};
use roc_gen_llvm::llvm::externs::add_default_roc_externs;
use roc_load::file::LoadingProblem;
use roc_load::file::MonomorphizedModule;
use roc_mono::ir::OptLevel;
use roc_parse::ast::Expr;
use roc_parse::parser::SyntaxError;
use roc_region::all::LineInfo;
use roc_target::TargetInfo;
use roc_types::pretty_print::{content_to_string, name_all_type_vars};

#[cfg(feature = "llvm")]
use {
    inkwell::context::Context, inkwell::module::Linkage, roc_build::link::module_to_dylib,
    roc_build::program::FunctionIterator,
};

use crate::app_memory::AppMemoryInternal;
use crate::eval::{self, ToAstProblem};

pub enum ReplOutput {
    Problems(Vec<String>),
    NoProblems { expr: String, expr_type: String },
}

pub fn gen_and_eval<'a>(
    src: &[u8],
    target: Triple,
    opt_level: OptLevel,
) -> Result<ReplOutput, SyntaxError<'a>> {
    if cfg!(feature = "llvm") {
        gen_and_eval_llvm(src, target, opt_level)
    } else {
        todo!("REPL must be compiled with LLVM feature for now")
    }
}

pub fn gen_and_eval_llvm<'a>(
    src: &[u8],
    target: Triple,
    opt_level: OptLevel,
) -> Result<ReplOutput, SyntaxError<'a>> {
    let arena = Bump::new();
    let target_info = TargetInfo::from(&target);

    let loaded = match compile_to_mono(&arena, src, target_info) {
        Ok(x) => x,
        Err(prob_strings) => {
            return Ok(ReplOutput::Problems(prob_strings));
        }
    };

    let MonomorphizedModule {
        procedures,
        entry_point,
        interns,
        exposed_to_host,
        mut subs,
        module_id: home,
        ..
    } = loaded;

    let context = Context::create();
    let builder = context.create_builder();
    let module = arena.alloc(roc_gen_llvm::llvm::build::module_from_builtins(
        &target, &context, "",
    ));

    // mark our zig-defined builtins as internal
    for function in FunctionIterator::from_module(module) {
        let name = function.get_name().to_str().unwrap();
        if name.starts_with("roc_builtins") {
            function.set_linkage(Linkage::Internal);
        }
    }

    debug_assert_eq!(exposed_to_host.values.len(), 1);
    let (main_fn_symbol, main_fn_var) = exposed_to_host.values.iter().next().unwrap();
    let main_fn_symbol = *main_fn_symbol;
    let main_fn_var = *main_fn_var;

    // pretty-print the expr type string for later.
    name_all_type_vars(main_fn_var, &mut subs);
    let content = subs.get_content_without_compacting(main_fn_var);
    let expr_type_str = content_to_string(content, &subs, home, &interns);

    let (_, main_fn_layout) = match procedures.keys().find(|(s, _)| *s == main_fn_symbol) {
        Some(layout) => *layout,
        None => {
            return Ok(ReplOutput::NoProblems {
                expr: "<function>".to_string(),
                expr_type: expr_type_str,
            });
        }
    };

    let module = arena.alloc(module);
    let (module_pass, function_pass) =
        roc_gen_llvm::llvm::build::construct_optimization_passes(module, opt_level);

    let (dibuilder, compile_unit) = roc_gen_llvm::llvm::build::Env::new_debug_info(module);

    // Compile and add all the Procs before adding main
    let env = roc_gen_llvm::llvm::build::Env {
        arena: &arena,
        builder: &builder,
        dibuilder: &dibuilder,
        compile_unit: &compile_unit,
        context: &context,
        interns,
        module,
        target_info,
        is_gen_test: true, // so roc_panic is generated
        // important! we don't want any procedures to get the C calling convention
        exposed_to_host: MutSet::default(),
    };

    // Add roc_alloc, roc_realloc, and roc_dealloc, since the repl has no
    // platform to provide them.
    add_default_roc_externs(&env);

    let (main_fn_name, main_fn) = roc_gen_llvm::llvm::build::build_procedures_return_main(
        &env,
        opt_level,
        procedures,
        entry_point,
    );

    env.dibuilder.finalize();

    // we don't use the debug info, and it causes weird errors.
    module.strip_debug_info();

    // Uncomment this to see the module's un-optimized LLVM instruction output:
    // env.module.print_to_stderr();

    if main_fn.verify(true) {
        function_pass.run_on(&main_fn);
    } else {
        panic!("Main function {} failed LLVM verification in build. Uncomment things nearby to see more details.", main_fn_name);
    }

    module_pass.run_on(env.module);

    // Uncomment this to see the module's optimized LLVM instruction output:
    // env.module.print_to_stderr();

    // Verify the module
    if let Err(errors) = env.module.verify() {
        panic!(
            "Errors defining module:\n{}\n\nUncomment things nearby to see more details.",
            errors.to_string()
        );
    }

    let lib = module_to_dylib(env.module, &target, opt_level)
        .expect("Error loading compiled dylib for test");

    let res_answer = unsafe {
        eval::jit_to_ast(
            &arena,
            lib,
            main_fn_name,
            main_fn_layout,
            content,
            &env.interns,
            home,
            &subs,
            target_info,
            &AppMemoryInternal,
        )
    };

    let formatted = format_answer(&arena, res_answer, expr_type_str);
    Ok(formatted)
}

fn format_answer(
    arena: &Bump,
    res_answer: Result<Expr, ToAstProblem>,
    expr_type_str: String,
) -> ReplOutput {
    let mut expr = roc_fmt::Buf::new_in(arena);

    use eval::ToAstProblem::*;
    match res_answer {
        Ok(answer) => {
            answer.format_with_options(&mut expr, Parens::NotNeeded, Newlines::Yes, 0);
        }
        Err(FunctionLayout) => {
            expr.indent(0);
            expr.push_str("<function>");
        }
    }

    ReplOutput::NoProblems {
        expr: expr.into_bump_str().to_string(),
        expr_type: expr_type_str,
    }
}

fn compile_to_mono<'a>(
    arena: &'a Bump,
    src: &[u8],
    target_info: TargetInfo,
) -> Result<MonomorphizedModule<'a>, Vec<String>> {
    use roc_reporting::report::{
        can_problem, mono_problem, type_problem, RocDocAllocator, DEFAULT_PALETTE,
    };

    // SAFETY: we've already verified that this is valid UTF-8 during parsing.
    let src_str: &str = unsafe { from_utf8_unchecked(src) };

    let stdlib = arena.alloc(roc_builtins::std::standard_stdlib());
    let filename = PathBuf::from("REPL.roc");
    let src_dir = Path::new("fake/test/path");

    let module_src = arena.alloc(promote_expr_to_module(src_str));

    let exposed_types = MutMap::default();
    let loaded = roc_load::file::load_and_monomorphize_from_str(
        arena,
        filename,
        module_src,
        stdlib,
        src_dir,
        exposed_types,
        target_info,
        builtin_defs_map,
    );

    let mut loaded = match loaded {
        Ok(v) => v,
        Err(LoadingProblem::FormattedReport(report)) => {
            return Err(vec![report]);
        }
        Err(e) => {
            panic!("error while loading module: {:?}", e)
        }
    };

    let MonomorphizedModule {
        interns,
        sources,
        can_problems,
        type_problems,
        mono_problems,
        ..
    } = &mut loaded;

    let mut lines = Vec::new();

    for (home, (module_path, src)) in sources.iter() {
        let can_probs = can_problems.remove(home).unwrap_or_default();
        let type_probs = type_problems.remove(home).unwrap_or_default();
        let mono_probs = mono_problems.remove(home).unwrap_or_default();

        let error_count = can_probs.len() + type_probs.len() + mono_probs.len();

        if error_count == 0 {
            continue;
        }

        let line_info = LineInfo::new(module_src);
        let src_lines: Vec<&str> = src.split('\n').collect();
        let palette = DEFAULT_PALETTE;

        // Report parsing and canonicalization problems
        let alloc = RocDocAllocator::new(&src_lines, *home, interns);

        for problem in can_probs.into_iter() {
            let report = can_problem(&alloc, &line_info, module_path.clone(), problem);
            let mut buf = String::new();

            report.render_color_terminal(&mut buf, &alloc, &palette);

            lines.push(buf);
        }

        for problem in type_probs {
            if let Some(report) = type_problem(&alloc, &line_info, module_path.clone(), problem) {
                let mut buf = String::new();

                report.render_color_terminal(&mut buf, &alloc, &palette);

                lines.push(buf);
            }
        }

        for problem in mono_probs {
            let report = mono_problem(&alloc, &line_info, module_path.clone(), problem);
            let mut buf = String::new();

            report.render_color_terminal(&mut buf, &alloc, &palette);

            lines.push(buf);
        }
    }

    if !lines.is_empty() {
        Err(lines)
    } else {
        Ok(loaded)
    }
}

fn promote_expr_to_module(src: &str) -> String {
    let mut buffer =
        String::from("app \"app\" provides [ replOutput ] to \"./platform\"\n\nreplOutput =\n");

    for line in src.lines() {
        // indent the body!
        buffer.push_str("    ");
        buffer.push_str(line);
        buffer.push('\n');
    }

    buffer
}