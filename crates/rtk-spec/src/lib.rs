pub mod canonical;
pub mod v1alpha1;
pub mod merge_strategies;

#[doc(inline)]
pub use k8s_openapi::DeepMerge;

/// Like [`DeepMerge`], but for merging from another, similar type.
pub trait DeepMergeFrom<T> {
    fn merge_from(&mut self, other: T);
}
