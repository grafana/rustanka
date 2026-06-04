import { assert } from "@std/assert";
import {
	ArrValue,
	type ImportResolver,
	ObjValue,
	setErrorFactory,
	State,
	Val,
	ValKind,
} from "./lib/jsonnet_web.js";

export interface JrsonnetFrame {
	desc: string;
	path?: string;
	line?: number;
	column?: number;
}

export class JrsonnetError extends Error {
	override name = "JrsonnetError" as const;
	frames: JrsonnetFrame[];

	constructor(message: string, frames: JrsonnetFrame[], cause?: unknown) {
		super(message, cause !== undefined ? { cause } : undefined);
		this.frames = frames;
	}
}

setErrorFactory(
	(message: string, frames: JrsonnetFrame[], cause: unknown) =>
		new JrsonnetError(message, frames, cause),
);

export { ArrValue, type ImportResolver, ObjValue, State, Val, ValKind };

export class FetchImportResolver implements ImportResolver {
	constructor(base: URL | string) {
		this.#base = new URL(base);
	}

	#base: URL;
	#resolution = new Map<string, string>();
	#bytes = new Map<string, Uint8Array>();

	async resolveFrom(from: string | undefined, path: string): Promise<string> {
		const base = from !== undefined ? from : this.#base;
		const requestStr = new URL(path, base).toString();

		const cached = this.#resolution.get(requestStr);
		if (cached !== undefined) return cached;

		const resp = await fetch(requestStr);
		if (!resp.ok) {
			throw new Error(
				`fetch ${requestStr}: HTTP ${resp.status} ${resp.statusText}`,
			);
		}
		const canonical = resp.url;
		if (!this.#bytes.has(canonical)) {
			this.#bytes.set(canonical, await resp.bytes());
		}
		this.#resolution.set(requestStr, canonical);
		return canonical;
	}

	loadFileContents(resolved: string): Promise<Uint8Array> {
		const bytes = this.#bytes.get(resolved);
		assert(bytes, `not loaded: ${resolved}`);
		return Promise.resolve(bytes);
	}
}
