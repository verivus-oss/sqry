export interface User {
  id: string;
}

export function getUser(): Promise<User> {
  return fetchUser();
}

export const makeList = (): Array<string> => ["a", "b"];

export async function fetchValue(): Promise<number> {
  return 1;
}

function fetchUser(): Promise<User> {
  return Promise.resolve({ id: "123" });
}
