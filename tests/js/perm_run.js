// args: --allow-run=echo
import { assertRejects } from "./_assert.js";
const e = await assertRejects(() => harbor.run({ cmd: ["sh", "-c", "echo pwned"] }), harbor.errors.PermissionDenied);
console.log(e.message);
const r = await harbor.run({ cmd: ["echo", "hi", "there"] });
console.log(r.code, r.success, JSON.stringify(r.stdoutText()));
