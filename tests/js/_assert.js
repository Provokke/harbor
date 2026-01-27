// Tiny assertion helper shared by the integration corpus.
export function assert(cond, msg = "assertion failed") {
  if (!cond) throw new Error(msg);
}
export function assertEq(a, b, msg = "") {
  const sa = JSON.stringify(a), sb = JSON.stringify(b);
  if (sa !== sb) throw new Error(`assertEq failed ${msg}: ${sa} !== ${sb}`);
}
export async function assertRejects(fn, cls, msg = "") {
  try { await fn(); } catch (e) {
    if (cls && !(e instanceof cls)) throw new Error(`expected ${cls.name}, got ${e && e.name}: ${e && e.message} ${msg}`);
    return e;
  }
  throw new Error(`expected rejection ${msg}`);
}
export function assertThrows(fn, cls, msg = "") {
  try { fn(); } catch (e) {
    if (cls && !(e instanceof cls)) throw new Error(`expected ${cls.name}, got ${e && e.name} ${msg}`);
    return e;
  }
  throw new Error(`expected throw ${msg}`);
}
