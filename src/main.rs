use std::collections::BTreeMap;

use anyhow::{Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use lopdf::{Bookmark, Document, Object, ObjectId};

fn main() -> Result<()> {
    let matches = Command::new("pdf")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("merge")
                .about("Merge multiple PDFs into a single output PDF.")
                .arg(Arg::new("PDFs").action(ArgAction::Append))
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .default_value("output.pdf"),
                ),
        )
        .subcommand(
            Command::new("compress")
                .about("Compress a PDF to save disk space or make it easier to attach.")
                .arg(Arg::new("PDFs").action(ArgAction::Append)),
        )
        .get_matches();

    match matches.subcommand() {
        Some(("merge", data)) => merge_pdfs(data).with_context(|| "Failed to merge pdfs")?,
        Some(("compress", data)) => {
            compress_pdfs(data).with_context(|| "Failed to compress pdfs")?
        }
        _ => Err(anyhow::anyhow!("This command does not exist"))?,
    }

    Ok(())
}

fn compress_pdfs(data: &ArgMatches) -> Result<()> {
    println!("Checking input validity...");

    let  pdfs = data
        .get_many::<String>("PDFs")
        .with_context(|| "No PDFs found to merge")?
        .peekable();

    println!("Loading PDFs into memory...");

    let documents = pdfs
        .map(|f| {
            if f.ends_with(".pdf") {
                (
                    f.clone(),
                    Document::load(f)
                        .with_context(|| format!("File not found: {}", f))
                        .unwrap(),
                )
            } else {
                let mut s = String::new();
                s.push_str(f);
                s.push_str(".pdf");
                (
                    s.clone(),
                    Document::load(s.as_str())
                        .with_context(|| format!("File not found: {}", f))
                        .unwrap(),
                )
            }
        })
        .collect::<Vec<_>>();

    println!("Compressing PDFs...");

    for (name, mut doc) in documents {
        let mut compressed_name: String = name[0..(name.len() - 4)].into();
        compressed_name.push_str("_compressed.pdf");

        println!("Compressing {name:?} to {compressed_name:?}");

        doc.compress();
        doc.save(compressed_name)
            .with_context(|| "Failed to save file")?;
    }

    println!("🦀 All done! 🦀");

    Ok(())
}

fn merge_pdfs(data: &ArgMatches) -> Result<()> {
    let output = match data.get_one::<String>("output") {
        Some(s) => {
            if s.ends_with(".pdf") {
                s.clone()
            } else {
                let mut s = s.clone();
                s.push_str(".pdf");
                s
            }
        }
        None => "output.pdf".into(),
    };

    let mut pdfs = data
        .get_many::<String>("PDFs")
        .with_context(|| "No PDFs found to merge")?
        .peekable();

    println!("Checking input validity...");

    if pdfs.peek().is_none() {
        Err(anyhow::anyhow!("No pdfs found"))?;
    }

    println!("Loading PDFs into memory...");

    let documents = pdfs
        .map(|f| {
            if f.ends_with(".pdf") {
                Document::load(f)
                    .with_context(|| format!("File not found: {}", f))
                    .unwrap()
            } else {
                let mut s = String::new();
                s.push_str(f);
                s.push_str(".pdf");
                Document::load(s.as_str())
                    .with_context(|| format!("File not found: {}", f))
                    .unwrap()
            }
        })
        .collect::<Vec<_>>();

    println!("Merging {} PDFs into {}...", documents.len(), output);

    let mut max_id = 1;
    let mut pagenum = 1;
    let mut documents_pages = BTreeMap::new();
    let mut documents_objects = BTreeMap::new();
    let mut document = Document::with_version("1.5");

    for mut doc in documents {
        let mut first = false;
        doc.renumber_objects_with(max_id);

        max_id = doc.max_id + 1;

        documents_pages.extend(
            doc.get_pages()
                .into_values()
                .map(|object_id| {
                    if !first {
                        let bookmark = Bookmark::new(
                            format!("Page_{}", pagenum),
                            [0.0, 0.0, 1.0],
                            0,
                            object_id,
                        );
                        document.add_bookmark(bookmark, None);
                        first = true;
                        pagenum += 1;
                    }

                    (object_id, doc.get_object(object_id).unwrap().to_owned())
                })
                .collect::<BTreeMap<ObjectId, Object>>(),
        );
        documents_objects.extend(doc.objects);
    }

    // Catalog and Pages are mandatory
    let mut catalog_object: Option<(ObjectId, Object)> = None;
    let mut pages_object: Option<(ObjectId, Object)> = None;

    // Process all objects except "Page" type
    for (object_id, object) in documents_objects.iter() {
        // We have to ignore "Page" (as are processed later), "Outlines" and "Outline" objects
        // All other objects should be collected and inserted into the main Document
        match object.type_name().unwrap_or("") {
            "Catalog" => {
                // Collect a first "Catalog" object and use it for the future "Pages"
                catalog_object = Some((
                    if let Some((id, _)) = catalog_object {
                        id
                    } else {
                        *object_id
                    },
                    object.clone(),
                ));
            }
            "Pages" => {
                // Collect and update a first "Pages" object and use it for the future "Catalog"
                // We have also to merge all dictionaries of the old and the new "Pages" object
                if let Ok(dictionary) = object.as_dict() {
                    let mut dictionary = dictionary.clone();
                    if let Some((_, ref object)) = pages_object {
                        if let Ok(old_dictionary) = object.as_dict() {
                            dictionary.extend(old_dictionary);
                        }
                    }

                    pages_object = Some((
                        if let Some((id, _)) = pages_object {
                            id
                        } else {
                            *object_id
                        },
                        Object::Dictionary(dictionary),
                    ));
                }
            }
            "Page" => {}     // Ignored, processed later and separately
            "Outlines" => {} // Ignored, not supported yet
            "Outline" => {}  // Ignored, not supported yet
            _ => {
                document.objects.insert(*object_id, object.clone());
            }
        }
    }

    // If no "Pages" object found abort
    if pages_object.is_none() {
        println!("Pages root not found.");

        return Ok(());
    }

    // Iterate over all "Page" objects and collect into the parent "Pages" created before
    for (object_id, object) in documents_pages.iter() {
        if let Ok(dictionary) = object.as_dict() {
            let mut dictionary = dictionary.clone();
            dictionary.set("Parent", pages_object.as_ref().unwrap().0);

            document
                .objects
                .insert(*object_id, Object::Dictionary(dictionary));
        }
    }

    // If no "Catalog" found abort
    if catalog_object.is_none() {
        println!("Catalog root not found.");

        return Ok(());
    }

    let catalog_object = catalog_object.unwrap();
    let pages_object = pages_object.unwrap();

    // Build a new "Pages" with updated fields
    if let Ok(dictionary) = pages_object.1.as_dict() {
        let mut dictionary = dictionary.clone();

        // Set new pages count
        dictionary.set("Count", documents_pages.len() as u32);

        // Set new "Kids" list (collected from documents pages) for "Pages"
        dictionary.set(
            "Kids",
            documents_pages
                .into_iter()
                .map(|(object_id, _)| Object::Reference(object_id))
                .collect::<Vec<_>>(),
        );

        document
            .objects
            .insert(pages_object.0, Object::Dictionary(dictionary));
    }

    // Build a new "Catalog" with updated fields
    if let Ok(dictionary) = catalog_object.1.as_dict() {
        let mut dictionary = dictionary.clone();
        dictionary.set("Pages", pages_object.0);
        dictionary.remove(b"Outlines"); // Outlines not supported in merged PDFs

        document
            .objects
            .insert(catalog_object.0, Object::Dictionary(dictionary));
    }

    document.trailer.set("Root", catalog_object.0);

    // Update the max internal ID as wasn't updated before due to direct objects insertion
    document.max_id = document.objects.len() as u32;

    // Reorder all new Document objects
    document.renumber_objects();

    //Set any Bookmarks to the First child if they are not set to a page
    document.adjust_zero_pages();

    //Set all bookmarks to the PDF Object tree then set the Outlines to the Bookmark content map.
    if let Some(n) = document.build_outline() {
        if let Ok(Object::Dictionary(ref mut dict)) = document.get_object_mut(catalog_object.0) {
            dict.set("Outlines", Object::Reference(n));
        }
    }

    document.compress();

    println!("Writing output file...");

    document
        .save(&output)
        .with_context(|| format!("Failed to write output file {}", output))?;

    println!("🦀 All done! 🦀");

    Ok(())
}

// A simple PDF tool to merge files, etc.
// #[derive(Parser, Debug)]
// #[command(version)]
// struct MyArgs {
//     /// List of pdfs to merge, using space delimiter
//     pdfs: Vec<String>,
//     /// Output pdf file name
//     #[arg(short, long, default_value = "output.pdf")]
//     output: String,
// }

// fn old_main() -> Result<()> {
//     let args = MyArgs::parse();

//     println!("Checking input validity...");

//     if args.pdfs.is_empty() {
//         Err(anyhow::anyhow!("No pdfs found"))?;
//     }

//     println!("Merging {} pdfs into {}...", args.pdfs.len(), args.output);

//     let mut merge_doc = Document::new();

//     args.pdfs
//         .iter()
//         .map(|f| {
//             if f.ends_with(".pdf") {
//                 Document::load(f)
//                     .with_context(|| format!("File not found: {}", f))
//                     .unwrap()
//             } else {
//                 let mut f = f.clone();
//                 f.push_str(".pdf");
//                 Document::load(f.as_str())
//                     .with_context(|| format!("File not found: {}", f))
//                     .unwrap()
//             }
//         })
//         .for_each(|doc| {
//             doc.get_pages().into_iter().for_each(|page| {
//                 merge_doc.add_object(page.1);
//             });
//         });

//     println!("Writing output file...");

//     merge_doc
//         .save(&args.output)
//         .with_context(|| format!("Failed to write output file {}", args.output))?;

//     println!("All done! 🦀");

//     Ok(())
// }
