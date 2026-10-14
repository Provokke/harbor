// exit: 1
// stderr: error: Uncaught (in promise) Error: nobody catches me
console.log("start");
Promise.reject(new Error("nobody catches me"));
setTimeout(() => console.log("SHOULD NOT RUN"), 50);
