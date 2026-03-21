export const answer = 42;

export function run(): void {
  console.log("running");
}

export default class Controller {
  start(): void {
    run();
  }
}

export interface User {
  id: string;
  name: string;
}

export type Callback = (value: number) => void;

export { Helpers } from "./helpers";
