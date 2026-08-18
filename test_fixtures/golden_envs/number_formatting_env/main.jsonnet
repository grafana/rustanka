// How numbers survive being exported.
//
// Every Jsonnet number is a float64, and both tk and rtk reach YAML the same
// way: the Jsonnet implementation manifests the value as JSON text, that text is
// parsed back into a float64, and a YAML library formats the float64. So the
// exported text is chosen by the YAML library, not by Jsonnet — tk's is
// gopkg.in/yaml.v2, which formats with strconv.FormatFloat(f, 'g', -1, 64).
//
// Nothing else in test_fixtures covers this: float_rounding_env looks like it
// does, but it exercises std.format, which produces a *string* and so never
// reaches the float formatting at all. These are numbers, exported as scalars.

{
  apiVersion: 'example.com/v1',
  kind: 'NumberFormatting',
  metadata: {
    name: 'numbers',
  },
  spec: {
    // Integers, and a float that happens to be whole. Whether the last one
    // keeps a decimal point is the YAML library's choice.
    zero: 0,
    one: 1,
    negativeOne: -1,
    wholeFloat: 1.0,

    // The results everyone knows are not exact. Both need all 17 significant
    // digits to round-trip.
    pointOnePlusPointTwo: 0.1 + 0.2,
    oneThird: 1 / 3,

    // Go's 'g' formatting switches to an exponent once the decimal exponent
    // reaches 6, and below -4. These sit either side of both edges.
    justBelowMillion: 999999,
    million: 1000000,
    justAboveMillion: 1000001,
    tenMillion: 1e7,
    smallBoundary: 0.0001,
    belowSmallBoundary: 0.00001,

    // Well past either edge.
    veryLarge: 1e100,
    verySmall: 1e-100,

    // Where float64 stops counting in ones. The second of these is not
    // representable and lands on the first.
    twoPow53: 9007199254740992,
    twoPow53Plus1: 9007199254740993,

    // Larger than an int64 can hold, so anything treating these as integers
    // rather than floats has to say so.
    int64Max: 9223372036854775807,
    beyondInt64: 9223372036854775808,

    // Enough digits to be truncated by a formatter that assumes fewer.
    manyDigits: 1.2345678901234567,
    negativeFraction: -0.5,

    // The same choices again, nested and inside a sequence, in case the
    // formatter treats those paths differently.
    nested: {
      value: 2.5,
      exponent: 1e21,
    },
    list: [1, 1.5, 1000000, 0.0001, 0.1 + 0.2],
  },
}
