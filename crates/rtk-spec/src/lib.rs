pub mod canonical;
pub mod merge_strategies;
pub mod v1alpha1;

#[doc(inline)]
pub use k8s_openapi::DeepMerge;

/// Like [`DeepMerge`], but for merging from another, similar type.
pub trait DeepMergeFrom<T> {
	fn merge_from(&mut self, other: T);
}

impl<T> DeepMergeFrom<T> for T
where
	T: DeepMerge,
{
	fn merge_from(&mut self, other: T) {
		DeepMerge::merge_from(self, other);
	}
}
