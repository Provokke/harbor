import { a, callB } from "./modules/cycle_a.js";
import { count, inc, area } from "./modules/counter.ts";
import { incFromDeep, where } from "./modules/deep/deeper/up.js";
import data from "./modules/data.json";
import { ready } from "./modules/tla.js";
import * as ns from "./modules/counter.ts";

console.log(a, callB());
inc(); incFromDeep();
console.log("live binding count:", count, ns.count, "same module instance:", ns.inc === inc);
console.log("area", area({ kind: "circle", r: 2 }), area({ kind: "square", s: 3 }));
console.log(where, data.name, data.list.length, ready);
console.log("main?", import.meta.main, import.meta.url.endsWith("modules_graph.js"));
const dyn = await import("./modules/counter.ts");
console.log("dynamic import shares instance:", dyn.count === 2);
const dynTla = await import("./modules/tla.js");
console.log(dynTla.ready);
try { await import("./modules/does_not_exist.js"); } catch (e) { console.log("missing dyn import:", e.message.includes("Module not found")); }
try { await import("left-pad"); } catch (e) { console.log("bare:", e.message); }
