use hex::encode;
use jrsonnet_evaluator::{IStr, function::builtin};

#[builtin]
pub fn builtin_md5(s: IStr) -> String {
	format!("{:x}", md5::compute(s.as_bytes()))
}

#[builtin]
pub fn builtin_sha1(str: IStr) -> String {
	use sha1::digest::Digest;
	encode(sha1::Sha1::digest(str.as_bytes()))
}

#[builtin]
pub fn builtin_sha256(str: IStr) -> String {
	use sha2::digest::Digest;
	encode(sha2::Sha256::digest(str.as_bytes()))
}

#[builtin]
pub fn builtin_sha512(str: IStr) -> String {
	use sha2::digest::Digest;
	encode(sha2::Sha512::digest(str.as_bytes()))
}

#[builtin]
pub fn builtin_sha3(str: IStr) -> String {
	use sha3::digest::Digest;
	encode(sha3::Sha3_512::digest(str.as_bytes()))
}
