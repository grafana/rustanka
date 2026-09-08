use std::cmp::Ordering;

pub fn compare_string_keys(a: &str, b: &str) -> Ordering {
	let ar: Vec<char> = a.chars().collect();
	let br: Vec<char> = b.chars().collect();
	let mut digits = false;

	for i in 0..ar.len().min(br.len()) {
		if ar[i] == br[i] {
			digits = ar[i].is_ascii_digit();
			continue;
		}

		let al = ar[i].is_alphabetic();
		let bl = br[i].is_alphabetic();
		if al && bl {
			return ar[i].cmp(&br[i]);
		}
		if al || bl {
			return if digits {
				if al {
					Ordering::Less
				} else {
					Ordering::Greater
				}
			} else if bl {
				Ordering::Less
			} else {
				Ordering::Greater
			};
		}

		let mut an: i64 = 0;
		let mut bn: i64 = 0;
		if ar[i] == '0' || br[i] == '0' {
			let mut j = i;
			while j > 0 && ar[j - 1].is_ascii_digit() {
				j -= 1;
				if ar[j] != '0' {
					an = 1;
					bn = 1;
					break;
				}
			}
		}

		let mut ai = i;
		while ai < ar.len() && ar[ai].is_ascii_digit() {
			an = an.wrapping_mul(10).wrapping_add(ar[ai] as i64 - '0' as i64);
			ai += 1;
		}
		let mut bi = i;
		while bi < br.len() && br[bi].is_ascii_digit() {
			bn = bn.wrapping_mul(10).wrapping_add(br[bi] as i64 - '0' as i64);
			bi += 1;
		}

		if an != bn {
			return an.cmp(&bn);
		}
		if ai != bi {
			return ai.cmp(&bi);
		}
		return ar[i].cmp(&br[i]);
	}

	ar.len().cmp(&br.len())
}
