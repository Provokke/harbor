// Errors from ops carry JS stack traces and proper classes; caught errors don't exit.
try {
  harbor.readTextFileSync("/nope");
} catch (e) {
  console.log(e.name, e instanceof harbor.errors.PermissionDenied, e instanceof Error);
  console.log(e.stack.includes("error_caught.js:3"));
}
try { null.x; } catch (e) { console.log(e.constructor.name); }
try { harbor.close(9999); } catch (e) { console.log(e.name, e.message); }
await harbor.core.opAsync("op_read", 12345, new Uint8Array(1)).catch((e) => console.log("async", e.name));
console.log("survived");
