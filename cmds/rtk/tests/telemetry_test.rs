use std::{
	io::{BufRead, BufReader, Read, Write},
	net::TcpListener,
	process::Command,
	time::{Duration, Instant},
};

#[rstest::rstest]
#[case(None)]
#[case(Some("http/protobuf"))]
fn exporter_delivers_traces_without_profiling(#[case] protocol: Option<&str>) {
	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	let address = listener.local_addr().unwrap();
	listener.set_nonblocking(true).unwrap();
	let collector = std::thread::spawn(move || {
		let deadline = Instant::now() + Duration::from_secs(15);
		let mut stream = loop {
			match listener.accept() {
				Ok((stream, _)) => break stream,
				Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
					assert!(Instant::now() < deadline, "no OTLP request received");
					std::thread::sleep(Duration::from_millis(10));
				}
				Err(error) => panic!("failed to accept OTLP request: {error}"),
			}
		};
		stream
			.set_read_timeout(Some(Duration::from_secs(5)))
			.unwrap();
		let mut reader = BufReader::new(&mut stream);
		let mut request = String::new();
		let mut content_length = None;
		loop {
			let mut line = String::new();
			assert!(reader.read_line(&mut line).unwrap() > 0);
			if line == "\r\n" {
				break;
			}
			if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
				content_length = Some(length.trim().parse::<usize>().unwrap());
			}
			request.push_str(&line);
		}
		let mut body = vec![0; content_length.expect("OTLP request has a content length")];
		reader.read_exact(&mut body).unwrap();
		stream
			.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
			.unwrap();
		(request, body)
	});

	let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../test_fixtures/golden_envs/number_formatting_env");
	let output_dir = tempfile::tempdir().unwrap();
	let mut command = Command::new(env!("CARGO_BIN_EXE_rtk"));
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("OTEL_")
			|| name.to_string_lossy().starts_with("PYROSCOPE_")
		{
			command.env_remove(name);
		}
	}
	command
		.current_dir(fixture)
		.args(["--log-level=info", "export"])
		.arg(output_dir.path())
		.arg(".")
		.env("PYROSCOPE_ENABLED", "false")
		.env("NO_PROXY", "*")
		.env("OTEL_TRACES_SAMPLER", "always_on")
		.env("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{address}"));
	if let Some(protocol) = protocol {
		command.env("OTEL_EXPORTER_OTLP_PROTOCOL", protocol);
	}
	let output = command.output().unwrap();
	let stderr = String::from_utf8_lossy(&output.stderr);
	assert!(output.status.success(), "{stderr}");
	assert!(!stderr.contains("panicked"), "{stderr}");
	let (request, body) = collector.join().unwrap();
	assert!(
		request.starts_with("POST /v1/traces HTTP/1.1\r\n"),
		"{request}"
	);
	assert!(
		request
			.to_ascii_lowercase()
			.contains("content-type: application/x-protobuf"),
		"{request}"
	);
	assert!(!body.is_empty());
}
