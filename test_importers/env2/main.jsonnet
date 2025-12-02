local helper = import '../lib/helper.libsonnet';

{
  message: helper.greet('env2'),
}

