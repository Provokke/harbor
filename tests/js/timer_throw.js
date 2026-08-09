// exit: 1
// stderr: error: Uncaught RangeError: from timer
setTimeout(() => console.log("first"), 0);
setTimeout(() => { throw new RangeError("from timer"); }, 1);
setTimeout(() => console.log("never"), 20);
