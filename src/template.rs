use std::collections::HashMap;

pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut rendered = template.to_string();

    for (key, value) in vars {
        let token = format!("{{{{{}}}}}", key);
        rendered = rendered.replace(&token, value);
    }

    rendered
}
