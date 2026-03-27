use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::jsonnet::evaluator::{
	Evaluation, Evaluator, EvaluatorImplementation, EvaluatorOptions, GlobalEvaluatorOptions,
	JrsonnetEvaluator,
};

#[derive(Clone, Debug)]
pub enum ConfigurableEvaluator {
	Jrsonnet(JrsonnetEvaluator),
}

impl Evaluator for ConfigurableEvaluator {
	fn new(options: GlobalEvaluatorOptions) -> Self {
		match options.implementation {
			EvaluatorImplementation::Jrsonnet | EvaluatorImplementation::Binary(_) => {
				ConfigurableEvaluator::Jrsonnet(JrsonnetEvaluator::new(options))
			}
		}
	}

	#[inline]
	fn global_options(&self) -> &GlobalEvaluatorOptions {
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.global_options(),
		}
	}

	#[inline]
	fn collect_cycles(&self) {
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.collect_cycles(),
		}
	}

	#[inline]
	fn clear_thread_local_state(&self) {
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.clear_thread_local_state(),
		}
	}

	#[inline]
	fn eval_file<P>(&self, path: P, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		P: AsRef<Path>,
	{
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.eval_file(path, opts),
		}
	}

	#[inline]
	fn eval_snippet<S>(&self, snippet: S, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		S: AsRef<str>,
	{
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.eval_snippet(snippet, opts),
		}
	}

	#[inline]
	fn eval_snippet_with_jpath<S>(
		&self,
		snippet: S,
		jpath: Vec<PathBuf>,
		opts: &EvaluatorOptions,
	) -> Result<Evaluation>
	where
		S: AsRef<str>,
	{
		match self {
			ConfigurableEvaluator::Jrsonnet(e) => e.eval_snippet_with_jpath(snippet, jpath, opts),
		}
	}
}
