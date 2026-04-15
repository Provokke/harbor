// args: --allow-read=$TMP --allow-write=$TMP
import { assert, assertEq, assertRejects } from "./_assert.js";
const dir = harbor.args[0];
const f = `${dir}/hello.txt`;
await harbor.writeTextFile(f, "line1\n");
await harbor.writeTextFile(f, "line2\n", { append: true });
console.log(JSON.stringify(await harbor.readTextFile(f)));
const bytes = await harbor.readFile(f);
console.log(bytes.constructor.name, bytes.length, bytes[0]);
harbor.writeTextFileSync(`${dir}/sync.txt`, "sync!");
console.log(harbor.readTextFileSync(`${dir}/sync.txt`));
await harbor.writeFile(`${dir}/bin.dat`, new Uint8Array([0, 1, 2, 255]));
console.log(Array.from(await harbor.readFile(`${dir}/bin.dat`)));
const st = await harbor.stat(f);
console.log("stat", st.size, st.isFile, st.isDirectory);
await harbor.mkdir(`${dir}/a/b/c`, { recursive: true });
console.log("dir", (await harbor.stat(`${dir}/a/b`)).isDirectory);
const e = await assertRejects(() => harbor.readTextFile(`${dir}/missing.txt`), harbor.errors.NotFound);
console.log("missing:", e.name);
await assertRejects(() => harbor.mkdir(`${dir}/a`), harbor.errors.AlreadyExists);

// file handles + resource table
const file = await harbor.open(`${dir}/handle.txt`, { write: true, create: true, truncate: true, read: true });
await file.write(new TextEncoder().encode("0123456789"));
file.seek(2);
const buf = new Uint8Array(4);
const n = await file.read(buf);
console.log("read", n, new TextDecoder().decode(buf));
console.log("resources", JSON.stringify(harbor.resources()));
file.close();
console.log("after close", JSON.stringify(harbor.resources()));
await assertRejects(() => file.read(buf), harbor.errors.BadResource);

// streaming a big file through the async-iterator adapter
const big = new Uint8Array(1_000_000);
harbor.core.ops.bufFill(big, 7, 13);
await harbor.writeFile(`${dir}/big.bin`, big);
const h = await harbor.open(`${dir}/big.bin`);
let total = 0, chunks = 0, sum = 0;
for await (const chunk of h.readable) {
  total += chunk.length; chunks++;
  sum += harbor.core.ops.bufSum(chunk);
}
console.log("streamed", total, "bytes in", chunks, "chunks; checksum ok:", sum === harbor.core.ops.bufSum(big));
console.log("stream closed file:", JSON.stringify(harbor.resources()));
// getReader API
const h2 = await harbor.open(f);
const reader = h2.readable.getReader();
const first = await reader.read();
console.log("reader", first.done, new TextDecoder().decode(first.value).trim().split("\n"));
console.log("reader end", (await reader.read()).done);
await harbor.remove(`${dir}/a`, { recursive: true });
await assertRejects(() => harbor.stat(`${dir}/a`), harbor.errors.NotFound);
console.log("removed");
