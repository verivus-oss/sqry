// SPDX-License-Identifier: MIT
export class SessionState {
  id = "";
  status = "new";
  static cacheKey = "session";
  #token = "";
  sharedName = 0;

  constructor(id, token) {
    this.id = id;
    this.#token = token;
    this.createdAt = Date.now();
  }

  touch() {
    this.status = "active";
    return this.id;
  }
}

export class AuditState {
  sharedName = 1;
}
