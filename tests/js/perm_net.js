// args: --allow-net=127.0.0.1:1
import { assertRejects, assertThrows } from "./_assert.js";
const PD = harbor.errors.PermissionDenied;
let e = await assertRejects(() => harbor.connect({ hostname: "127.0.0.1", port: 80 }), PD);
console.log("connect:", e.message);
await assertRejects(() => harbor.connect({ hostname: "example.com", port: 1 }), PD);
console.log("connect other host denied");
e = assertThrows(() => harbor.listen({ hostname: "127.0.0.1", port: 0 }), PD);
console.log("listen:", e.message);
await assertRejects(() => fetch("http://127.0.0.1:8080/"), PD);
console.log("fetch denied");
// allowed host:port passes the permission check (and then fails to connect)
e = await assertRejects(() => harbor.connect({ hostname: "127.0.0.1", port: 1 }));
console.log("allowed entry gets past permissions:", e.name !== "PermissionDenied");
