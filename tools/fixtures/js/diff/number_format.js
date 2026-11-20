// Number to string for exponents: 1e21 and above, 1e-7 and below use the exponent form (js2 writes all the digits).
var xs = [1e21, 1e-7, 5e-324, 1.7976931348623157e308, 123456789012345680000];
for (var i = 0; i < xs.length; i++) print(String(xs[i]));
