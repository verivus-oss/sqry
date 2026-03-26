import { expect } from "chai";
import { IndexQueue } from "../src/indexQueue";

describe("IndexQueue", () => {
  it("deduplicates concurrent runs", async () => {
    const queue = new IndexQueue();
    let count = 0;

    const task = () => {
      count += 1;
      return new Promise<void>((resolve) => setTimeout(resolve, 10));
    };

    await Promise.all([
      queue.run("workspace", task),
      queue.run("workspace", task),
    ]);

    expect(count).to.equal(1);
  });

  it("runs sequential tasks after completion", async () => {
    const queue = new IndexQueue();
    let count = 0;

    await queue.run("workspace", async () => {
      count += 1;
    });
    await queue.run("workspace", async () => {
      count += 1;
    });

    expect(count).to.equal(2);
  });
});
