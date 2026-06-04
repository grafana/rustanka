import { assertEquals } from "@std/assert";
import { format, FormatOptions } from "./fmt.ts";

Deno.test("format", () => {
	const opts = new FormatOptions();
	assertEquals(format("{a:1+1}", opts), "{ a: 1 + 1 }\n");
});
