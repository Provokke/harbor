// exit: 1
// stderr: error: Uncaught PermissionDenied: Requires read access to "/etc/passwd"
// stderr: at file://$DIR/perm_uncaught.js:5:
console.log("before");
await harbor.readTextFile("/etc/passwd");
console.log("never printed");
