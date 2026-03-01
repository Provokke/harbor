import { assert, assertEq, assertThrows } from "./_assert.js";
const enc = new TextEncoder();
const dec = new TextDecoder();
const s = "héllo, 世界 🚢";
const bytes = enc.encode(s);
console.log(bytes.length, bytes instanceof Uint8Array, enc.encoding);
assertEq(dec.decode(bytes), s);
assertEq(Array.from(enc.encode("A€")), [65, 0xe2, 0x82, 0xac]);
// subarray views decode only their window
assertEq(dec.decode(bytes.subarray(0, 5)), "héll");
// ArrayBuffer and DataView inputs
assertEq(dec.decode(enc.encode("ab").buffer), "ab");
// lossy vs fatal
const bad = new Uint8Array([0x61, 0xff, 0x62]);
assertEq(dec.decode(bad), "a�b");
assertThrows(() => new TextDecoder("utf-8", { fatal: true }).decode(bad), TypeError);
assertThrows(() => new TextDecoder("latin1"), RangeError);
const dest = new Uint8Array(3);
assertEq(enc.encodeInto("abcdef", dest).written, 3);
assertEq(dec.decode(dest), "abc");
assertEq(dec.decode(), "");
console.log("encoding ok");
