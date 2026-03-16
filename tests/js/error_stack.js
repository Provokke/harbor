// exit: 1
// stderr: error: Uncaught Error: deep failure
// stderr: at inner (file://$DIR/error_stack.js:9:9)
// stderr: at middle (file://$DIR/error_stack.js:12:3)
// stderr: at outer (file://$DIR/error_stack.js:15:3)
console.log("start");

function inner() {
  throw new Error("deep failure");
}
function middle() {
  inner();
}
function outer() {
  middle();
}
setTimeout(outer, 1);
