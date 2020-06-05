extern crate dreammaker as dm;
extern crate dmm_tools;
extern crate rayon;
extern crate structopt;

extern crate serde;
extern crate serde_json;

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::RwLock;
use std::collections::HashSet;

use self::dm::objtree::ObjectTree;
use self::dmm_tools::*;

use jobs;

// ----------------------------------------------------------------------------

byond_fn! { render_map(map_path) {
    invoker(map_path).ok()
} }

// Returns new job-id.
byond_fn! { start_render_job(map_path) {
    let map_path = map_path.to_string();
    Some(jobs::start(move || {
        match invoker(&map_path) {
            Ok(r) => r,
            Err(e) => e.to_string()
        }
    }))
} }

// Checks status of a job
byond_fn! { check_render_job(id) {
    Some(jobs::check(id))
} }

// Actual invoker for SpacemanDMM
fn invoker(path: &str) -> Result<String, String> {
    let mut context = Context::default();
    let cmd = Command::Minimap {
        output: "data/nanomaps".to_string(),
        min: None,
        max: None,
        enable: "".to_string(),
        disable: "".to_string(),
        pngcrush: false,
        optipng: false,
        files: vec![path.to_string()],
    };

    let opt = Opt {
        environment: Some("paradise.dme".to_string()),
        command: cmd,
        jobs: 8,
    };

    context.dm_context.set_print_severity(Some(dm::Severity::Error));
    rayon::ThreadPoolBuilder::new()
        .num_threads(opt.jobs)
        .build_global()
        .expect("failed to initialize thread pool");
    context.parallel = opt.jobs != 1;

    run(&opt, &opt.command, &mut context);
    Ok("SUCCESS".to_string())
}

// Everything below is part of the main render core of SpacemanDMM, converted to be used within a library instead of a binary

#[derive(Default)]
struct Context {
    dm_context: dm::Context,
    objtree: ObjectTree,
    icon_cache: IconCache,
    exit_status: AtomicIsize,
    parallel: bool,
    procs: bool,
}

impl Context {
    fn objtree(&mut self, opt: &Opt) {
        let environment = match opt.environment {
            Some(ref env) => env.into(),
            None => match dm::detect_environment_default() {
                Ok(Some(found)) => found,
                _ => dm::DEFAULT_ENV.into(),
            },
        };
        println!("parsing {}", environment.display());

        if let Some(parent) = environment.parent() {
            self.icon_cache.set_icons_root(&parent);
        }

        self.dm_context.autodetect_config(&environment);
        let pp = match dm::preprocessor::Preprocessor::new(&self.dm_context, environment) {
            Ok(pp) => pp,
            Err(e) => {
                eprintln!("i/o error opening environment:\n{}", e);
                std::process::exit(1);
            }
        };
        let indents = dm::indents::IndentProcessor::new(&self.dm_context, pp);
        let mut parser = dm::parser::Parser::new(&self.dm_context, indents);
        if self.procs {
            parser.enable_procs();
        }
        self.objtree = parser.parse_object_tree();
    }
}

struct Opt {
    environment: Option<String>,
    jobs: usize,
    command: Command,
}

// ----------------------------------------------------------------------------
// Subcommands

enum Command {
    Minimap {
        output: String,
        min: Option<CoordArg>,
        max: Option<CoordArg>,
        enable: String,
        disable: String,
        pngcrush: bool,
        optipng: bool,
        files: Vec<String>,
    },
}

fn run(opt: &Opt, command: &Command, context: &mut Context) {
    match *command {
        // --------------------------------------------------------------------
        Command::Minimap {
            ref output, min, max, ref enable, ref disable, ref files,
            pngcrush, optipng,
        } => {
            context.objtree(opt);
            if context
                .dm_context
                .errors()
                .iter()
                .filter(|e| e.severity() <= dm::Severity::Error)
                .next()
                .is_some()
            {
                println!("there were some parsing errors; render may be inaccurate")
            }
            let Context {
                ref objtree,
                ref icon_cache,
                ref exit_status,
                parallel,
                ..
            } = *context;

            let render_passes = &dmm_tools::render_passes::configure(enable, disable);
            let paths: Vec<&Path> = files.iter().map(|p| p.as_ref()).collect();
            let errors: RwLock<HashSet<String>> = Default::default();

            let perform_job = move |path: &Path| {
                let mut filename;
                let prefix = if parallel {
                    filename = path.file_name().unwrap().to_string_lossy().into_owned();
                    filename.push_str(": ");
                    println!("{}{}", filename, path.display());
                    &filename
                } else {
                    println!("{}", path.display());
                    "    "
                };

                let map = match dmm::Map::from_file(path) {
                    Ok(map) => map,
                    Err(e) => {
                        eprintln!("Failed to load {}:\n{}", path.display(), e);
                        exit_status.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                let (dim_x, dim_y, dim_z) = map.dim_xyz();
                let mut min = min.unwrap_or(CoordArg { x: 0, y: 0, z: 0 });
                let mut max = max.unwrap_or(CoordArg {
                    x: dim_x + 1,
                    y: dim_y + 1,
                    z: dim_z + 1,
                });
                min.x = clamp(min.x, 1, dim_x);
                min.y = clamp(min.y, 1, dim_y);
                min.z = clamp(min.z, 1, dim_z);
                max.x = clamp(max.x, min.x, dim_x);
                max.y = clamp(max.y, min.y, dim_y);
                max.z = clamp(max.z, min.z, dim_z);
                println!("{}rendering from {} to {}", prefix, min, max);

                let do_z_level = |z| {
                    println!("{}generating z={}", prefix, 1 + z);
                    let bump = Default::default();
                    let minimap_context = minimap::Context {
                        objtree: &objtree,
                        map: &map,
                        level: map.z_level(z),
                        min: (min.x - 1, min.y - 1),
                        max: (max.x - 1, max.y - 1),
                        render_passes: &render_passes,
                        errors: &errors,
                        bump: &bump,
                    };
                    let image = minimap::generate(minimap_context, icon_cache).unwrap();
                    if let Err(e) = std::fs::create_dir_all(output) {
                        eprintln!("Failed to create output directory {}:\n{}", output, e);
                        exit_status.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    let outfile = format!(
                        "{}/{}_nanomap_z{}.png",
                        output,
                        path.file_stem().unwrap().to_string_lossy(),
                        1 + z
                    );
                    println!("{}saving {}", prefix, outfile);
                    image.to_file(outfile.as_ref()).unwrap();
                    if pngcrush {
                        println!("    pngcrush {}", outfile);
                        let temp = format!("{}.temp", outfile);
                        assert!(std::process::Command::new("pngcrush")
                            .arg("-ow")
                            .arg(&outfile)
                            .arg(&temp)
                            .stderr(std::process::Stdio::null())
                            .status()
                            .unwrap()
                            .success(), "pngcrush failed");
                    }
                    if optipng {
                        println!("{}optipng {}", prefix, outfile);
                        assert!(std::process::Command::new("optipng")
                            .arg(&outfile)
                            .stderr(std::process::Stdio::null())
                            .status()
                            .unwrap()
                            .success(), "optipng failed");
                    }
                };
                ((min.z - 1)..(max.z)).into_iter().for_each(do_z_level);
            };
            paths.into_iter().for_each(perform_job);
        },
        // --------------------------------------------------------------------
    }
}

// ----------------------------------------------------------------------------
// Argument parsing helpers

#[derive(Debug, Copy, Clone)]
struct CoordArg {
    x: usize,
    y: usize,
    z: usize,
}

impl fmt::Display for CoordArg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.z != 0 {
            write!(f, "{},{},{}", self.x, self.y, self.z)
        } else {
            write!(f, "{},{}", self.x, self.y)
        }
    }
}

impl std::str::FromStr for CoordArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s
            .split(",")
            .map(|x| x.parse())
            .collect::<Result<Vec<_>, std::num::ParseIntError>>()
        {
            Ok(ref vec) if vec.len() == 2 => Ok(CoordArg {
                x: vec[0],
                y: vec[1],
                z: 0,
            }),
            Ok(ref vec) if vec.len() == 3 => Ok(CoordArg {
                x: vec[0],
                y: vec[1],
                z: vec[2],
            }),
            Ok(_) => Err("must specify 2 or 3 coordinates".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn clamp(val: usize, min: usize, max: usize) -> usize {
    if val < min {
        min
    } else if val > max {
        max
    } else {
        val
    }
}
