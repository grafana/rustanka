use anyhow::Result;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use rand_core::OsRng;
use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};

pub(super) struct Key;

impl Key {
	pub(super) fn generate(kind: &str) -> Result<String> {
		Ok(match kind {
			"" | "rsa" => rsa::RsaPrivateKey::new(&mut OsRng, 4096)?
				.to_pkcs1_pem(LineEnding::LF)?
				.to_string(),
			"ecdsa" => p256::SecretKey::random(&mut OsRng)
				.to_sec1_pem(LineEnding::LF)?
				.to_string(),
			"ed25519" => ed25519_dalek::SigningKey::generate(&mut OsRng)
				.to_pkcs8_pem(LineEnding::LF)?
				.to_string(),
			"dsa" => Self::dsa(),
			_ => format!("Unknown type {kind}"),
		})
	}

	fn dsa() -> String {
		let components = dsa::Components::generate(&mut OsRng, dsa::KeySize::DSA_2048_256);
		let key = dsa::SigningKey::generate(&mut OsRng, components);
		let public = key.verifying_key();
		let components = public.components();
		let der = yasna::construct_der(|writer| {
			writer.write_sequence(|writer| {
				writer.next().write_i64(0);
				for value in [
					components.p(),
					components.q(),
					components.g(),
					public.y(),
					key.x(),
				] {
					writer.next().write_bigint_bytes(&value.to_bytes_be(), true);
				}
			});
		});
		pem::encode(&pem::Pem::new("DSA PRIVATE KEY", der))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ed25519_dalek::pkcs8::DecodePrivateKey;
	#[test]
	fn generated_keys_use_sprig_encodings() {
		let elliptic = Key::generate("ecdsa").unwrap();
		assert!(elliptic.starts_with("-----BEGIN EC PRIVATE KEY-----"));
		p256::SecretKey::from_sec1_pem(&elliptic).unwrap();
		let edwards = Key::generate("ed25519").unwrap();
		ed25519_dalek::SigningKey::from_pkcs8_pem(&edwards).unwrap();
		assert_eq!(Key::generate("unknown").unwrap(), "Unknown type unknown");
	}
}
