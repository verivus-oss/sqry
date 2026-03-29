type RecursivePartial<T> = {
  [P in keyof T]?: T[P] extends object ? RecursivePartial<T[P]> : T[P];
};

interface Nested<A, B extends A, C extends keyof B> {
  value: Pick<B, C>;
  transform: <T extends B>(input: T) => Partial<T>;
}

class GenericClass<T extends Record<string, unknown>> {
  process<K extends keyof T>(key: K): T[K] | Promise<T[K]> | undefined {
    return undefined;
  }
}
