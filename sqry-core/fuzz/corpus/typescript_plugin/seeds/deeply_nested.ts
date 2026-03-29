export namespace A {
  export namespace B {
    export namespace C {
      export namespace D {
        export namespace E {
          export class DeepClass {
            method1() {
              return function inner1() {
                return function inner2() {
                  return function inner3() {
                    return function inner4() {
                      return 42;
                    };
                  };
                };
              };
            }
          }
        }
      }
    }
  }
}
