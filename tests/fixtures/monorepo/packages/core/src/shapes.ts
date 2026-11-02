export enum Kind {
  Circle = "circle",
  Square = "square",
}

export interface Shape {
  readonly kind: Kind;
  area(): number;
}

export class Circle implements Shape {
  readonly kind = Kind.Circle;
  constructor(readonly radius: number) {}
  area(): number {
    return Math.PI * this.radius ** 2;
  }
}

export class Square implements Shape {
  readonly kind = Kind.Square;
  constructor(readonly side: number) {}
  area(): number {
    return this.side * this.side;
  }
}

export namespace Geometry {
  export function totalArea(shapes: readonly Shape[]): number {
    return shapes.map((shape) => shape.area()).reduce((a, b) => a + b, 0);
  }
}
