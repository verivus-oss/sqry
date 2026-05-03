export interface Box<T extends object> {
  value: T;
}

export function identity<T extends string>(value: T): T {
  return value;
}

export function map<T, U extends number>(items: T[], f: (value: T) => U): U[] {
  return items.map(f);
}

export type Mapped<T, V = string> = {
  [K in keyof T]: V;
};

export type Variadic<T extends unknown[]> = [...T];
export type Conditional<T> = T extends string ? number : boolean;
