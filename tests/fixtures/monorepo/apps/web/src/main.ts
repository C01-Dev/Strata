import { Square, sum, GOLDEN_RATIO } from "@mono/core";
import { render, summary, unitCircle, type Label } from "@mono/ui";

const labels: Label[] = [
  { text: "unit", shape: unitCircle },
  { text: "big square", shape: new Square(4) },
];

export function page(): string {
  const sizes = sum(labels.map((label) => label.text.length));
  return `${render(labels)}\n${summary(labels.map((label) => label.shape))}\n${sizes * GOLDEN_RATIO}`;
}
