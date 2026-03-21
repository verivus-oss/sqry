export class IndexQueue {
  private readonly running = new Map<string, Promise<void>>();

  run(key: string, task: () => Promise<void>): Promise<void> {
    const existing = this.running.get(key);
    if (existing) {
      return existing;
    }

    const promise = task().finally(() => {
      this.running.delete(key);
    });

    this.running.set(key, promise);
    return promise;
  }
}
