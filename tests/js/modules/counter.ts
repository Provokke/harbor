export let count: number = 0;
export function inc(): void { count++; }
export type Shape = { kind: "circle"; r: number } | { kind: "square"; s: number };
export const area = (s: Shape): number => (s.kind === "circle" ? Math.round(Math.PI * s.r ** 2) : s.s ** 2);
