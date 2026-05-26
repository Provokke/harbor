// args: --allow-net=127.0.0.1
import { assertEq } from "./_assert.js";
const listener = harbor.listen({ hostname: "127.0.0.1", port: 0 });
const port = listener.addr.port;
console.log("listening on random port:", port > 1024);
// echo server: uppercases everything it receives via the stream adapter
(async () => {
  for await (const conn of listener) {
    (async () => {
      for await (const chunk of conn.readable) {
        const text = new TextDecoder().decode(chunk);
        if (text.startsWith("GET ")) {
          const body = JSON.stringify({ hello: "fetch", path: text.split(" ")[1] });
          await conn.write(new TextEncoder().encode(`HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: ${body.length}\r\n\r\n${body}`));
          conn.close();
          return;
        }
        await conn.write(new TextEncoder().encode(text.toUpperCase()));
      }
      conn.close();
    })();
  }
})();
const c = await harbor.connect({ hostname: "127.0.0.1", port });
await c.write(new TextEncoder().encode("hello over tcp"));
const buf = new Uint8Array(64);
const n = await c.read(buf);
console.log(new TextDecoder().decode(buf.subarray(0, n)));
c.close();
// three concurrent clients
const replies = await Promise.all(["a", "b", "c"].map(async (m) => {
  const conn = await harbor.connect({ hostname: "127.0.0.1", port });
  await conn.write(new TextEncoder().encode(`msg-${m}`));
  const b = new Uint8Array(16);
  const k = await conn.read(b);
  conn.close();
  return new TextDecoder().decode(b.subarray(0, k));
}));
console.log(replies.join(" "));
const res = await fetch(`http://127.0.0.1:${port}/api?x=1`);
console.log(res.status, res.ok, res.headers.get("content-type"));
assertEq(await res.json(), { hello: "fetch", path: "/api?x=1" });
console.log("fetch json ok");
try { await harbor.connect({ hostname: "127.0.0.1", port: 1 }); } catch (e) { console.log("refused:", e.name); }
listener.close();
console.log("listener closed, loop exits");
