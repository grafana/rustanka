local data = {
  objectArray: [
    { name: 'item1', value: 100 },
    { name: 'item2', value: 200 },
  ],
};

local manifestYamlFromJson = std.native('manifestYamlFromJson');
local parseYaml = std.native('parseYaml');

local jsonString = std.toString(data);
local yamlString = manifestYamlFromJson(jsonString);
local parsed = parseYaml(yamlString);

{
  success: true,
  yaml: yamlString,
  parsed: parsed,
}


