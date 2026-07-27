// args: --allow-read --allow-write=$TMP/out
import { assertRejects, assertThrows } from "./_assert.js";
const PD = harbor.errors.PermissionDenied;
const dir = harbor.args[0];
await harbor.mkdir(`${dir}/out`);
await harbor.writeTextFile(`${dir}/out/ok.txt`, "ok");
console.log("inside scope:", await harbor.readTextFile(`${dir}/out/ok.txt`));
const outside = {
  writeTextFile: () => harbor.writeTextFile(`${dir}/nope.txt`, "x"),
  writeFile: () => harbor.writeFile(`${dir}/nope.bin`, new Uint8Array(1)),
  dotdotEscape: () => harbor.writeTextFile(`${dir}/out/../escape.txt`, "x"),
  prefixTrick: () => harbor.writeTextFile(`${dir}/outside.txt`, "x"),
  mkdir: () => harbor.mkdir(`${dir}/d`),
  remove: () => harbor.remove(dir, { recursive: true }),
  openCreate: () => harbor.open(`${dir}/c.txt`, { write: true, create: true }),
};
for (const [name, fn] of Object.entries(outside)) {
  await assertRejects(fn, PD, name);
  console.log(name, "denied");
}
assertThrows(() => harbor.writeTextFileSync("/tmp/harbor-nope.txt", "x"), PD);
console.log("writeTextFileSync denied");
// nothing leaked to disk
await assertRejects(() => harbor.stat(`${dir}/escape.txt`), harbor.errors.NotFound);
console.log("no file created");
