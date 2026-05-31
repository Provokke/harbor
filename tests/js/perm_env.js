// args: --allow-env=HARBOR_OK
import { assertEq, assertThrows } from "./_assert.js";
const PD = harbor.errors.PermissionDenied;
assertThrows(() => harbor.env.get("HOME"), PD);
assertThrows(() => harbor.env.set("OTHER", "1"), PD);
assertThrows(() => harbor.env.delete("HOME"), PD);
const e = assertThrows(() => harbor.env.toObject(), PD);
console.log("toObject:", e.message);
assertEq(harbor.env.get("HARBOR_OK"), "from-test");
harbor.env.set("HARBOR_OK", "changed");
assertEq(harbor.env.get("HARBOR_OK"), "changed");
harbor.env.delete("HARBOR_OK");
assertEq(harbor.env.get("HARBOR_OK"), undefined);
console.log("env allow-list ok");
