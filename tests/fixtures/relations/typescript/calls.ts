import defaultLogger from "./logger";
import { compute } from "./util";
import * as Helpers from "./helpers";

class Repository {
  static save(value: number): void {
    defaultLogger();
    console.info("saving", value);
  }
}

export class Service {
  process(value: number): void {
    defaultLogger();
    this.persist(value);
    Helpers.Hub.log(value);
    compute(value);
  }

  private persist(value: number): void {
    Repository.save(value);
  }
}

export function topLevel(): void {
  const service = new Service();
  service.process(1);
}
