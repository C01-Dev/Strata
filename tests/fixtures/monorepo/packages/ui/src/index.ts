import { Circle, Geometry, type Shape, mean } from "@mono/core";

export interface Label {
  text: string;
  shape: Shape;
}

/** One line per shape: "circle 3.14". */
export function render(labels: readonly Label[]): string {
  return labels.map((label) => `${label.shape.kind} ${label.shape.area().toFixed(2)}`).join("\n");
}

export function summary(shapes: readonly Shape[]): string {
  const average = mean(shapes.map((shape) => shape.area())) ?? 0;
  return `${shapes.length} shapes, total ${Geometry.totalArea(shapes)}, average ${average}`;
}

export const unitCircle = new Circle(1);
