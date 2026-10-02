import type { Shape } from "./modules/counter.ts";
import { area } from "./modules/counter.ts";

enum Dir { Up = 1, Down, Left = "L" }
interface Animal { name: string; speak(): string }
abstract class Base<T> {
  protected constructor(public readonly value: T) {}
  abstract describe(): string;
}
class Dog extends Base<number> implements Animal {
  name = "rex";
  private secret?: string;
  constructor(v: number) { super(v); }
  speak(): string { return `${this.name} says woof x${this.value}`; }
  describe() { return "dog"; }
}
function first<T extends unknown[]>(xs: T): T[0] { return xs[0]; }
const shape = { kind: "square", s: 4 } satisfies Shape;
const n = <number>(<unknown>"5" as any) as number;
declare const NEVER_EMITTED: string;
namespace Util { export const twice = (x: number): number => x * 2; }
let maybe: string | undefined;
console.log(Dir.Up, Dir.Down, Dir.Left, Dir[2]);
console.log(new Dog(3).speak(), new Dog(1).describe());
console.log(first<[string, number]>(["a", 1]), area(shape), n, Util.twice(21), maybe!);
console.log(typeof NEVER_EMITTED === "undefined" ? "declare stripped" : "?");
