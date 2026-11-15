// Numeric loop
let s = 0;
for (let i = 0; i < 5000000; i++) s = (s + i * i) % 1000003;
s;
