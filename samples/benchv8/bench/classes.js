// 类与方法调用
class Vec {
  constructor(x, y) { this.x = x; this.y = y; }
  add(o) { return new Vec(this.x + o.x, this.y + o.y); }
  get len2() { return this.x * this.x + this.y * this.y; }
}
let v = new Vec(0, 0);
const d = new Vec(1, 2);
for (let i = 0; i < 500000; i++) v = v.add(d);
v.len2;
