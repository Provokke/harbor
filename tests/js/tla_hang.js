// exit: 1
// stderr: Top-level await promise never resolved
await new Promise(() => {});
