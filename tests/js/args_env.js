// args: --allow-env
// argv: one|two words|3
console.log(JSON.stringify(harbor.args));
console.log(harbor.env.get("HARBOR_OK"), harbor.env.has("DEFINITELY_NOT_SET_123"));
console.log(typeof harbor.env.toObject().PATH);
console.log(Object.isFrozen(harbor), Object.isFrozen(harbor.args));
