// Defining module. Target: greet.
/** Says hi. See {@link greet} and {@link greet | the greeter}. */
export function greet(name: string): string {
  return "hi " + name;
}

export function twice(name: string): string {
  return greet(greet(name));
}

export const kind = typeof greet;
