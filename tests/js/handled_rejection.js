// Rejections that get a handler within the same turn are not reported.
// (Order follows the microtask queue: p2 reaction is queued before p.catch runs.)
const p = Promise.reject(new Error("late"));
queueMicrotask(() => p.catch((e) => console.log("caught", e.message)));
const p2 = (async () => { throw new TypeError("async fn"); })();
p2.then(null, (e) => console.log("caught", e.name));
setTimeout(() => console.log("exit normally"), 5);
