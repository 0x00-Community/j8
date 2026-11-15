// Closure creation and calls
function make(k) { let c = 0; return (x) => (c += x * k); }
let t = 0;
for (let i = 0; i < 1000; i++) {
  const f = make(i & 3);
  for (let j = 0; j < 1000; j++) t = f(j) % 100000;
}
t;
