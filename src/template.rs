use std::collections::HashMap;

pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut rendered = template.to_string();

    for (key, value) in vars {
        let token = format!("{{{{{}}}}}", key);
        rendered = rendered.replace(&token, value);
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_key_gets_replaced() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada".to_string());

        let rendered = render_template("Hello {{name}}", &vars);
        assert_eq!(rendered, "Hello Ada");
    }

    #[test]
    fn unknown_key_stays_unchanged() {
        let vars = HashMap::new();
        let rendered = render_template("Hello {{unknown}}", &vars);
        assert_eq!(rendered, "Hello {{unknown}}");
    }

    #[test]
    fn multiple_keys_in_one_template() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada".to_string());
        vars.insert("tool".to_string(), "cargo".to_string());

        let rendered = render_template("{{name}} uses {{tool}}", &vars);
        assert_eq!(rendered, "Ada uses cargo");
    }

    #[test]
    fn empty_vars_map_leaves_template_unchanged() {
        let vars = HashMap::new();
        let template = "Hello {{name}}, run {{tool}}.";
        let rendered = render_template(template, &vars);
        assert_eq!(rendered, template);
    }

    #[test]
    fn key_appears_multiple_times_replaced_everywhere() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada".to_string());

        let rendered = render_template("{{name}} and {{name}}", &vars);
        assert_eq!(rendered, "Ada and Ada");
    }

    #[test]
    fn empty_template_returns_empty_string() {
        let vars = HashMap::new();
        let rendered = render_template("", &vars);
        assert_eq!(rendered, "");
    }

    #[test]
    fn recursive_like_values_do_not_expand_again() {
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "{{y}}".to_string());

        let rendered = render_template("{{x}}", &vars);
        assert_eq!(rendered, "{{y}}");
    }

    #[test]
    fn special_char_key_supports_dots_and_spaces() {
        let mut vars = HashMap::new();
        vars.insert("my.key".to_string(), "dot-key".to_string());
        vars.insert("my key".to_string(), "space-key".to_string());

        let rendered = render_template("{{my.key}} {{my key}}", &vars);
        assert_eq!(rendered, "dot-key space-key");
    }
}
