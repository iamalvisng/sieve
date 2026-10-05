function Injectable() {
  return (t: unknown) => t;
}

@Injectable()
export class Service {
  run() {
    return 1;
  }
}
