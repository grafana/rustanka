local helper = import '../lib/helper.libsonnet';
local config = import 'config.libsonnet';

{
  message: helper.greet('env1'),
  config: config,
}

