// Object creation and property reads / writes
let sum = 0;
for (let i = 0; i < 300000; i++) {
  const p = { x: i, y: i * 2, z: { w: i & 7 } };
  p.x += p.z.w;
  sum += p.x + p.y;
}
sum;
