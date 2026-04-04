// exit: 7
setTimeout(() => console.log("never"), 1000);
console.log("exiting");
harbor.exit(7);
console.log("not printed");
