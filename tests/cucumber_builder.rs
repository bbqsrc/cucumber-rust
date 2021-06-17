extern crate cucumber_rust as cucumber;

use cucumber::{async_trait, criteria, World};
use futures::FutureExt;
use regex::Regex;
use std::{cell::RefCell, convert::Infallible};

pub struct MyWorld {
    // You can use this struct for mutable context in scenarios.
    foo: String,
    bar: usize,
    some_value: RefCell<u8>,
}

impl MyWorld {
    async fn test_async_fn(&mut self) {
        *self.some_value.borrow_mut() = 123u8;
        self.bar = 123;
    }
}

#[async_trait(?Send)]
impl World for MyWorld {
    type Error = Infallible;

    async fn new() -> Result<Self, Infallible> {
        Ok(Self {
            foo: "wat".into(),
            bar: 0,
            some_value: RefCell::new(0),
        })
    }
}

mod example_steps {
    use super::SomeString;
    use cucumber::{t, Steps, World};

    pub fn steps() -> Steps<crate::MyWorld> {
        let mut builder: Steps<crate::MyWorld> = Steps::new();

        builder
            .given_async(
                "a thing",
                t!(|mut world: crate::MyWorld, ctx| {
                    println!("{}", ctx.get::<&'static str>().unwrap());
                    println!("{}", ctx.get::<u32>().unwrap());
                    println!("{}", ctx.get::<SomeString>().unwrap().0);
                    println!("This is on stdout");
                    eprintln!("This is on stderr");
                    world.foo = "elho".into();
                    world.test_async_fn().await;
                    world
                }),
            )
            .when_regex_async(
                "something goes (.*)",
                t!(|_world, _ctx| crate::MyWorld::new().await.unwrap()),
            )
            .given(
                "I am trying out Cucumber",
                |mut world: crate::MyWorld, _ctx| {
                    world.foo = "Some string".to_string();
                    world
                },
            )
            .when("I consider what I am doing", |mut world, _ctx| {
                let new_string = format!("{}.", &world.foo);
                world.foo = new_string;
                world
            })
            .then("I am interested in ATDD", |world, _ctx| {
                assert_eq!(world.foo, "Some string.");
                world
            })
            .then_regex(r"^we can (.*) rules with regex$", |world, ctx| {
                // And access them as an array
                assert_eq!(ctx.matches[1], "implement");
                world
            })
            .given_regex(r"a number (\d+)", |mut world, ctx| {
                world.foo = ctx.matches[1].to_owned();
                world
            })
            .then_regex(r"twice that number should be (\d+)", |world, ctx| {
                let to_check = world.foo.parse::<i32>().unwrap();
                let expected = ctx.matches[1].parse::<i32>().unwrap();
                assert_eq!(to_check * 2, expected);
                world
            });

        builder
    }
}

struct SomeString(&'static str);

#[tokio::main]
async fn main() {
    // Do any setup you need to do before running the Cucumber runner.
    // e.g. setup_some_db_thing()?;

    cucumber::Cucumber::<MyWorld>::new()
        .features(&["./features/basic"])
        .steps(example_steps::steps())
        .context(
            cucumber::Context::new()
                .add("This is a string from the context.")
                .add(42u32)
                .add(SomeString("the newtype pattern helps here")),
        )
        .before(criteria::scenario(Regex::new(".*").unwrap()), |_| {
            async move {
                println!("S:AHHHH");
                ()
            }
            .boxed()
        })
        .after(criteria::scenario(Regex::new(".*").unwrap()), |_| {
            async move {
                println!("E:AHHHH");
            }
            .boxed()
        })
        .debug(true)
        .cli()
        .run_and_exit()
        .await
}
