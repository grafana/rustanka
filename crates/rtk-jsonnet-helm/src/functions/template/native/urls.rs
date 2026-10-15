use anyhow::{Result, ensure};
use gtmpl_ng::Template;
use serde_json::json;

pub(super) fn install(template: &mut Template) {
	template.add_func("urlParse", |arguments| {
		if arguments.len() != 1 {
			return Err(gtmpl_ng::FuncError::Generic(
				"urlParse needs one argument".into(),
			));
		}
		Ok(super::from_json(&Parts::parse(&arguments[0].to_string())?))
	});
	template.add_func("urlJoin", |arguments| {
		if arguments.len() != 1 {
			return Err(gtmpl_ng::FuncError::Generic(
				"urlJoin needs one argument".into(),
			));
		}
		Ok(Parts::join(&super::to_json(&arguments[0])?)?.into())
	});
}

struct Parts;
impl Parts {
	fn parse(text: &str) -> Result<serde_json::Value> {
		let explicit_scheme = text.find(':').is_some_and(|colon| {
			text[..colon].chars().enumerate().all(|(index, character)| {
				character.is_ascii_alphabetic()
					|| (index > 0
						&& (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
			})
		});
		let parsed = if explicit_scheme {
			url::Url::parse(text)?
		} else {
			url::Url::parse("http://placeholder.invalid/")?.join(text)?
		};

		let rest = if explicit_scheme {
			text.split_once(':').unwrap().1
		} else {
			text
		};
		let (body, fragment) = rest.split_once('#').unwrap_or((rest, ""));
		let (body, query) = body.split_once('?').unwrap_or((body, ""));
		let opaque = explicit_scheme && !rest.starts_with('/');
		let (authority, path) = if let Some(rest) = body.strip_prefix("//") {
			let end = rest.find('/').unwrap_or(rest.len());
			(&rest[..end], &rest[end..])
		} else {
			("", body)
		};
		let host = authority
			.rsplit_once('@')
			.map_or(authority, |(_, host)| host);
		let hostname = if host.starts_with('[') {
			host.split_once(']').map_or(host, |(host, _)| &host[1..])
		} else {
			host.split_once(':').map_or(host, |(host, _)| host)
		};
		let user = if parsed.username().is_empty() {
			String::new()
		} else {
			parsed.password().map_or_else(
				|| parsed.username().to_owned(),
				|password| format!("{}:{password}", parsed.username()),
			)
		};
		Ok(
			json!({"scheme":if explicit_scheme {text.split_once(':').unwrap().0} else {""}, "host":host, "hostname":hostname, "path":if opaque {String::new()} else {percent_encoding::percent_decode_str(path).decode_utf8_lossy().into_owned()}, "opaque":if opaque {body} else {""}, "query":query, "fragment":percent_encoding::percent_decode_str(fragment).decode_utf8_lossy(), "userinfo":user}),
		)
	}
	fn join(values: &serde_json::Value) -> Result<String> {
		ensure!(values.is_object(), "urlJoin needs a dictionary");
		let field = |name: &str| -> Result<&str> {
			match values.get(name) {
				None => Ok(""),
				Some(value) => value
					.as_str()
					.ok_or_else(|| anyhow::anyhow!("urlJoin {name} must be a string")),
			}
		};
		let scheme = field("scheme")?;
		let host = field("host")?;
		let opaque = field("opaque")?;
		let userinfo = field("userinfo")?;
		let mut output = if !opaque.is_empty() {
			format!("{scheme}:{opaque}")
		} else if !host.is_empty() {
			let prefix = if scheme.is_empty() {
				String::new()
			} else {
				format!("{scheme}:")
			};
			let credentials = if userinfo.is_empty() {
				String::new()
			} else {
				format!("{userinfo}@")
			};
			let mut parsed = url::Url::parse(&format!("http://{credentials}{host}"))?;
			parsed.set_path(field("path")?);
			format!(
				"{prefix}//{credentials}{host}{}",
				if field("path")?.is_empty() {
					""
				} else {
					parsed.path()
				}
			)
		} else {
			format!(
				"{}{}",
				if scheme.is_empty() {
					String::new()
				} else {
					format!("{scheme}:")
				},
				field("path")?
			)
		};
		let query = field("query")?;
		if !query.is_empty() {
			output.push('?');
			output.push_str(query);
		}
		let fragment = field("fragment")?;
		if !fragment.is_empty() {
			output.push('#');
			output.push_str(fragment);
		}
		Ok(output)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn url_fields_and_join_match_sprig() {
		let parts =
			Parts::parse("https://user:password@example.com:8443/a%20path?x=1#section").unwrap();
		assert_eq!(parts["host"], "example.com:8443");
		assert_eq!(
			Parts::parse("http://EXAMPLE.com:80").unwrap()["host"],
			"EXAMPLE.com:80"
		);
		assert_eq!(Parts::parse("http://EXAMPLE.com:80").unwrap()["path"], "");
		assert_eq!(parts["hostname"], "example.com");
		assert_eq!(parts["path"], "/a path");
		assert_eq!(parts["userinfo"], "user:password");
		assert_eq!(
			Parts::join(&parts).unwrap(),
			"https://user:password@example.com:8443/a%20path?x=1#section"
		);
		assert_eq!(Parts::parse("relative/path").unwrap()["scheme"], "");
		assert_eq!(
			Parts::parse("relative/path").unwrap()["path"],
			"relative/path"
		);
	}
}
